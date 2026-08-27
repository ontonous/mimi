use crate::ast::*;
use crate::codegen::expr::call::helpers::infer_generic_args;
use crate::codegen::types;
use crate::codegen::{call_try_basic_value, CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

/// is_empty (0.1.9): classify an arg's inferred type name into the Map-vs-Set
/// codegen kind. Maps (map_new -> Record) and sets ({...}) are both bare i64
/// handles at runtime; the call site's type is the only disambiguator.
fn classify_is_empty_kind(type_name: &str) -> Option<&'static str> {
    if type_name == "map" || type_name.starts_with("Map") || type_name == "Record" {
        Some("map")
    } else if type_name == "set" || type_name.starts_with("Set") {
        Some("set")
    } else {
        None
    }
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Depth-aware extraction of the `Ok` payload type name from a
    /// `Result<Ok, Err>` display name. Tolerates nested angle brackets and
    /// commas (product tuples `(i32, i32)`, nested containers
    /// `Result<Option<List<(i32, i32)>>, string>`) and the optional space after
    /// the separating comma emitted by `resolved_type_display_name`. Returns an
    /// empty string when `obj_type` is not a `Result<…>` (the caller then falls
    /// back to layout-based recovery).
    fn extract_result_ok_type(obj_type: &str) -> String {
        let inner = match obj_type.strip_prefix("Result<") {
            Some(i) => i,
            None => return String::new(),
        };
        let mut depth = 0i32;
        for (i, ch) in inner.char_indices() {
            match ch {
                '<' => depth += 1,
                '>' => depth -= 1,
                ',' if depth == 0 => return inner[..i].trim().to_string(),
                _ => {}
            }
        }
        String::new()
    }

    /// 0.39.136 architecture: SINGLE source of truth for typed to_json
    /// serialization dispatch (lists, sets, maps, records, product tuples,
    /// Option/Result wrappers, and every nested-product combination).
    /// Consumed by BOTH the legacy emitter (`compile_call`) and the resolved
    /// native emitter — previously three drifting inline copies of this
    /// routing existed, so a shape fixed in one silently stayed broken in the
    /// others. Keyed by an explicit type display name + LLVM argument value;
    /// no AST or hint-table access. Returns Ok(None) when the shape is not
    /// handled here and the caller should fall through.
    pub(in crate::codegen) fn emit_typed_to_json_dispatch(
        &mut self,
        obj_type: &str,
        arg0: BasicMetadataValueEnum<'ctx>,
        actual_ty: Option<BasicTypeEnum<'ctx>>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        // --- Recursive generator (true architectural fix, Phase A).
        // Handles scalars, string, Option, Result, List, Tuple, Record with a
        // single slot-based serializer per type — replacing the 99 bespoke
        // runtime functions + combinatorial dispatch tree. Map/Set/enum fall
        // through to the legacy per-combination tree until Phase B lands. ---
        if let Some(v) = self.try_emit_json_recursive(obj_type, &arg0, actual_ty)? {
            return Ok(Some(v));
        }
        // Product tuples: JSON array via recursive field serialization.
        // Only when the *source* type is a product tuple — never Option
        // `{i1,T}`, Result, enum `{i32,i64}`, string, or list layouts.
        // `arg_is_option_shape`: a `{i1, payload}` (2-field) struct — used to
        // route value-shaped Options into the Option branch even when the
        // resolved type display name is not `Option<…>` (e.g. the `None`
        // variant, which surfaces as a bare variant name).
        let arg_is_option_shape = match &arg0 {
            BasicMetadataValueEnum::StructValue(sv) => {
                let f = sv.get_type().get_field_types();
                f.len() == 2
                    && matches!(f[0], BasicTypeEnum::IntType(it) if it.get_bit_width() == 1)
            }
            _ => false,
        };
        if let BasicMetadataValueEnum::StructValue(sv) = arg0 {
            let fields = sv.get_type().get_field_types();
            let looks_like_option = !fields.is_empty()
                && matches!(
                    fields[0],
                    BasicTypeEnum::IntType(it) if it.get_bit_width() == 1
                );
            let is_string = fields.len() == 2
                && matches!(fields[0], BasicTypeEnum::PointerType(_))
                && matches!(
                    fields[1],
                    BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                );
            let is_list = fields.len() == 2
                && matches!(
                    fields[0],
                    BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                )
                && matches!(fields[1], BasicTypeEnum::PointerType(_));
            let is_enum_tag = fields.len() == 2
                && matches!(
                    fields[0],
                    BasicTypeEnum::IntType(it) if it.get_bit_width() == 32
                )
                && matches!(
                    fields[1],
                    BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                );
            let src_ty = obj_type.to_string();
            // Type aliases like `type Pair = (i32, i32)` keep the alias
            // name in var_type_names; resolve so product dispatch fires.
            let src_resolved = self.resolve_alias_type_name(&src_ty);
            let named_as_tuple = src_resolved.starts_with('(')
                || src_ty.starts_with('(')
                || src_ty.contains("Tuple")
                || self.is_product_tuple_alias(&src_ty);
            // Prefer AST-ish type name when available; else multi-field
            // product that is not option/string/list/enum.
            // Named records stay on the record path (type_defs Record);
            // product-tuple aliases are not "blocking" names.
            let blocks_product = self
                .type_defs
                .get(&src_ty)
                .is_some_and(|td| !matches!(td.kind, crate::ast::TypeDefKind::Alias(_)));
            if named_as_tuple
                || (fields.len() >= 2
                    && !looks_like_option
                    && !is_string
                    && !is_list
                    && !is_enum_tag
                    && !src_resolved.starts_with("Option")
                    && !src_resolved.starts_with("Result")
                    && !src_resolved.starts_with("List")
                    && !src_resolved.starts_with("Map")
                    && !src_resolved.starts_with("Set")
                    && !blocks_product)
            {
                let raw = self.emit_product_tuple_to_json(sv)?;
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
        }
        let obj_type = obj_type.to_string();
        if obj_type == "List" || obj_type.starts_with("List<") {
            let inner_opt = obj_type
                .strip_prefix("List<")
                .and_then(|s| s.strip_suffix('>'));
            let inner = inner_opt.unwrap_or("i64");
            let list_struct_ty = self.list_struct_type();
            let alloca = self.build_alloca(list_struct_ty, "to_json_list_alloca")?;
            match &arg0 {
                BasicMetadataValueEnum::StructValue(sv) => {
                    self.build_store(alloca, *sv)?;
                }
                BasicMetadataValueEnum::PointerValue(pv) => {
                    let loaded = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(list_struct_ty),
                            *pv,
                            "to_json_list_load",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_struct_value();
                    self.build_store(alloca, loaded)?;
                }
                _ => {
                    return Err(CompileError::Generic(format!(
                        "to_json: unexpected List argument kind for {}",
                        obj_type
                    )))
                }
            }
            // Check for record element type — needs callback-based serialization
            let is_record = self
                .type_defs
                .get(inner)
                .map(|td| matches!(td.kind, TypeDefKind::Record(_)))
                .unwrap_or(false);
            if is_record {
                if let Some(td) = self.type_defs.get(inner) {
                    if let TypeDefKind::Record(fields) = &td.kind {
                        let fields_clone = fields.clone();
                        return Ok(Some(self.compile_record_list_to_json(
                            inner,
                            &fields_clone,
                            &alloca,
                        )?));
                    }
                }
            }
            if inner.starts_with("List") {
                // Nested List: product-tuple inner uses codegen loop;
                // scalar/nested inners use element-type-aware
                // formatting. AUDIT FIX (H-18): the leaf formatter was
                // hardcoded to mimi_list_i64_to_json — f64 bit patterns
                // and string pointers were serialized as integers
                // (silently wrong JSON; VM emits correct values).
                let mid_elem = Self::strip_list_element_type(inner)
                    .or_else(|| {
                        inner
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();
                if mid_elem.starts_with('(') {
                    let raw = self.emit_list_list_product_tuple_to_json(alloca, &mid_elem)?;
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                let raw = self.emit_list_to_json_cstr(alloca, inner)?;
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            // List of Map of product / nested product Map values.
            if inner.starts_with("Map<") {
                let mode = self.map_nested_product_mode(inner);
                if mode >= 10 {
                    let raw = self.emit_list_map_nested_product_to_json(alloca, inner)?;
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
            }
            // List of Set of product / Set of Result of product.
            if let Some(set_elem) = inner.strip_prefix("Set<").and_then(|s| s.strip_suffix('>')) {
                if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                    let resolved = if self.is_product_tuple_alias(set_elem) {
                        self.resolve_alias_type_name(set_elem)
                    } else {
                        set_elem.to_string()
                    };
                    let mut arity: i64 = 0;
                    let mut depth = 0i32;
                    let mut any = false;
                    let body = resolved
                        .strip_prefix('(')
                        .and_then(|s| s.strip_suffix(')'))
                        .unwrap_or(resolved.as_str());
                    for ch in body.chars() {
                        match ch {
                            '<' | '(' => depth += 1,
                            '>' | ')' => depth -= 1,
                            ',' if depth == 0 => {
                                arity += 1;
                                any = true;
                            }
                            c if !c.is_whitespace() => any = true,
                            _ => {}
                        }
                    }
                    if any {
                        arity += 1;
                    }
                    let func = self.get_runtime_fn("mimi_list_set_product_to_json")?;
                    let raw = self
                        .build_call(
                            func,
                            &[
                                BasicMetadataValueEnum::PointerValue(alloca),
                                BasicMetadataValueEnum::IntValue(
                                    self.context
                                        .i64_type()
                                        .const_int(arity.max(1) as u64, false),
                                ),
                            ],
                            "list_set_product_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list set product to_json void")?
                        .into_pointer_value();
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                if let Some(opt_inner) = set_elem
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner) {
                        let resolved = if self.is_product_tuple_alias(opt_inner) {
                            self.resolve_alias_type_name(opt_inner)
                        } else {
                            opt_inner.to_string()
                        };
                        let raw =
                            self.emit_list_set_option_product_to_json(alloca, &resolved, 0)?;
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                }
                if set_elem.starts_with("Map<string, ") {
                    if let Some(val_ty) = set_elem
                        .strip_prefix("Map<string, ")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                            let resolved = if self.is_product_tuple_alias(val_ty) {
                                self.resolve_alias_type_name(val_ty)
                            } else {
                                val_ty.to_string()
                            };
                            let raw =
                                self.emit_list_set_map_product_to_json(alloca, &resolved, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                    }
                }
                if set_elem.starts_with("Result<") {
                    if let Some(ok_ty) = set_elem.strip_prefix("Result<").and_then(|s| {
                        let mut depth = 0i32;
                        for (i, ch) in s.char_indices() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    return Some(s[..i].trim());
                                }
                                _ => {}
                            }
                        }
                        None
                    }) {
                        if ok_ty.starts_with('(') || self.is_product_tuple_alias(ok_ty) {
                            let resolved = if self.is_product_tuple_alias(ok_ty) {
                                self.resolve_alias_type_name(ok_ty)
                            } else {
                                ok_ty.to_string()
                            };
                            let raw =
                                self.emit_list_set_result_product_to_json(alloca, &resolved, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                    }
                }
            }
            // List of Option<X> (e.g. `List<Option<List<(i32, i32)>>>` /
            // `List<Option<(i32, i32)>>` / `List<Option<i64>>`): the bytecode VM
            // serializes each element via its own `to_json` (wrapping `None` as
            // `"None"` and `Some(v)` as `{"Some":[<v>]}`). Mirror that here with a
            // per-element recursive dispatch instead of falling through to the
            // scalar `mimi_list_i64_to_json` helper, which mis-serialized tuple /
            // list elements as raw integers → wrong JSON or segfault.
            if inner.starts_with("Option") && !inner.contains("Map<") {
                let opt_elem_ty =
                    crate::codegen::extract_list_elem_type(&format!("List<{}>", inner))
                        .ok_or_else(|| {
                            CompileError::LlvmError(format!(
                                "to_json: cannot resolve element type for {}",
                                obj_type
                            ))
                        })?;
                let opt_bty = self.llvm_type_for(&opt_elem_ty).ok_or_else(|| {
                    CompileError::LlvmError(format!("to_json: no llvm type for element {}", inner))
                })?;
                let opt_struct_ty = match opt_bty {
                    BasicTypeEnum::StructType(s) => s,
                    _ => {
                        return Err(CompileError::LlvmError(format!(
                            "to_json: expected struct for Option element {}, got {:?}",
                            inner, opt_bty
                        )))
                    }
                };
                let opt_ptr_ty = opt_struct_ty.ptr_type(inkwell::AddressSpace::default());
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                // Generate (once per element type) an internal callback
                // `i8* cb(i8* elem)` that serializes one `Option<X>` element.
                let cb_ty =
                    i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
                let mut cb_name: String = "mimi_list_option_".to_string();
                for c in inner.chars() {
                    if c.is_alphanumeric() {
                        cb_name.push(c);
                    } else {
                        cb_name.push('_');
                    }
                }
                cb_name.push_str("_to_json");
                let callback = if let Some(existing) = self.module.get_function(&cb_name) {
                    existing
                } else {
                    let cb_fn = self.module.add_function(
                        &cb_name,
                        cb_ty,
                        Some(inkwell::module::Linkage::Internal),
                    );
                    let entry = self.context.append_basic_block(cb_fn, "entry");
                    let saved = self.builder.get_insert_block();
                    self.builder.position_at_end(entry);
                    let elem = cb_fn
                        .get_first_param()
                        .ok_or_else(|| {
                            CompileError::LlvmError("to_json Option callback: missing param".into())
                        })?
                        .into_pointer_value();
                    let opt_ptr = self
                        .builder
                        .build_bit_cast(elem, opt_ptr_ty, "cb_opt_ptr")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_pointer_value();
                    let res = self.emit_typed_to_json_dispatch(
                        inner,
                        BasicMetadataValueEnum::PointerValue(opt_ptr),
                        None,
                    )?;
                    let raw = match res {
                        Some(j) => match j {
                            BasicValueEnum::PointerValue(p) => p,
                            BasicValueEnum::StructValue(s) => self
                                .build_extract_value(s.into(), 0, "cb_opt_json_ptr")?
                                .into_pointer_value(),
                            other => other.into_pointer_value(),
                        },
                        None => {
                            return Err(CompileError::LlvmError(format!(
                                "to_json: Option<{}> callback dispatch returned None",
                                inner
                            )))
                        }
                    };
                    self.builder
                        .build_return(Some(&raw))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(saved.unwrap());
                    cb_fn
                };
                let callback_ptr = callback.as_global_value().as_pointer_value();
                let list_list_fn_ty = i8_ptr_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                        BasicMetadataTypeEnum::PointerType(
                            cb_ty.ptr_type(inkwell::AddressSpace::default()),
                        ),
                    ],
                    false,
                );
                let list_list_fn = self
                    .module
                    .get_function("mimi_list_list_to_json")
                    .unwrap_or_else(|| {
                        self.module.add_function(
                            "mimi_list_list_to_json",
                            list_list_fn_ty,
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                let raw = self
                    .build_call(
                        list_list_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(alloca),
                            BasicMetadataValueEnum::PointerValue(callback_ptr),
                        ],
                        "list_opt_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_list_list_to_string returned void")?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            let rt_fn_name = if inner.starts_with("Map") {
                if inner.contains("Map<string, string>") {
                    "mimi_list_map_to_json_string"
                } else {
                    // i32/i64/bool/f64 maps share int-style JSON objects.
                    "mimi_list_map_to_string"
                }
            } else if inner.starts_with("Set") {
                "mimi_list_set_to_json"
            } else if inner.starts_with("Option") && inner.contains("Map<") {
                // List of Option of Map — nested product modes via helper.
                let mode = if inner.contains("Map<string, string>") {
                    1i64
                } else if inner.contains("Map<string, bool>") {
                    2
                } else if inner.contains("Map<string, f64>") || inner.contains("Map<string, f32>") {
                    3
                } else {
                    self.map_nested_product_mode(inner)
                };
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let fn_ty = i8_ptr_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                        BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                );
                let callee = self
                    .module
                    .get_function("mimi_list_option_map_to_json")
                    .unwrap_or_else(|| {
                        self.module.add_function(
                            "mimi_list_option_map_to_json",
                            fn_ty,
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                let raw = self
                    .build_call(
                        callee,
                        &[
                            BasicMetadataValueEnum::PointerValue(alloca),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "to_json_list_opt_map",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("list option map to_json void")?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            } else if inner.starts_with("Option") {
                // Option of Result / product / record needs full Option
                // layout — never the scalar {i1,i64} runtime helper.
                // Exclude bare Option of scalar i32 (no Result/tuple/record).
                let opt_inner = inner
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                    .unwrap_or("");
                // List of Option of Set of product.
                if opt_inner.starts_with("Set<") {
                    if let Some(elem) = opt_inner
                        .strip_prefix("Set<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                            let resolved = if self.is_product_tuple_alias(elem) {
                                self.resolve_alias_type_name(elem)
                            } else {
                                elem.to_string()
                            };
                            let mut arity: i64 = 0;
                            let mut depth = 0i32;
                            let mut any = false;
                            let body = resolved
                                .strip_prefix('(')
                                .and_then(|s| s.strip_suffix(')'))
                                .unwrap_or(resolved.as_str());
                            for ch in body.chars() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        arity += 1;
                                        any = true;
                                    }
                                    c if !c.is_whitespace() => any = true,
                                    _ => {}
                                }
                            }
                            if any {
                                arity += 1;
                            }
                            let raw =
                                self.emit_list_option_set_product_to_json(alloca, arity.max(1))?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                    }
                }
                let needs_full = opt_inner.starts_with("Result")
                    || opt_inner.starts_with("List")
                    || opt_inner.starts_with("Set")
                    || opt_inner.starts_with('(')
                    || opt_inner.contains("Tuple")
                    || self
                        .type_defs
                        .get(opt_inner)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
                    || self.is_product_tuple_alias(opt_inner);
                if needs_full {
                    let raw = self.emit_list_option_product_tuple_to_json(alloca, inner)?;
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                "mimi_list_option_i64_to_json"
            } else if inner.starts_with("Result") && inner.contains("Set<") {
                // List of Result of Set of product — dedicated runtime path.
                if let Some(set_elem) = inner
                    .strip_prefix("Result<")
                    .and_then(|s| {
                        let mut depth = 0i32;
                        for (i, ch) in s.char_indices() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    return Some(s[..i].trim());
                                }
                                _ => {}
                            }
                        }
                        None
                    })
                    .and_then(|s| s.strip_prefix("Set<"))
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                        let elem = if self.is_product_tuple_alias(set_elem) {
                            self.resolve_alias_type_name(set_elem)
                        } else {
                            set_elem.to_string()
                        };
                        let raw = self.emit_list_result_set_product_runtime(alloca, &elem, 0)?;
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                }
                "mimi_list_result_i64_to_json"
            } else if inner.starts_with("Result") && inner.contains("Map<") {
                // List of Result of Map of product — dedicated runtime path.
                if let Some(val_ty) = inner
                    .strip_prefix("Result<")
                    .and_then(|s| {
                        let mut depth = 0i32;
                        for (i, ch) in s.char_indices() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    return Some(s[..i].trim());
                                }
                                _ => {}
                            }
                        }
                        None
                    })
                    .and_then(|s| s.strip_prefix("Map<string, "))
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                        let elem = if self.is_product_tuple_alias(val_ty) {
                            self.resolve_alias_type_name(val_ty)
                        } else {
                            val_ty.to_string()
                        };
                        let raw = self.emit_list_result_map_product_runtime(alloca, &elem, 0)?;
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                }
                // List of Result of Map — typed map Ok payload (scalars).
                // mode 0-3 scalars; mode 20+arity for product Map
                // (runtime list_result adds +10 for scalar map path).
                let mode = if inner.contains("Map<string, string>") {
                    1i64
                } else if inner.contains("Map<string, bool>") {
                    2
                } else if inner.contains("Map<string, f64>") || inner.contains("Map<string, f32>") {
                    3
                } else if let Some(val_ty) = inner
                    .strip_prefix("Result<")
                    .and_then(|s| {
                        let mut depth = 0i32;
                        for (i, ch) in s.char_indices() {
                            match ch {
                                '<' => depth += 1,
                                '>' => depth -= 1,
                                ',' if depth == 0 => {
                                    return Some(s[..i].trim());
                                }
                                _ => {}
                            }
                        }
                        None
                    })
                    .and_then(|s| s.strip_prefix("Map<string, "))
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                        let elem = if self.is_product_tuple_alias(val_ty) {
                            self.resolve_alias_type_name(val_ty)
                        } else {
                            val_ty.to_string()
                        };
                        let mut arity: i64 = 0;
                        let mut depth = 0i32;
                        let mut any = false;
                        let body = elem
                            .strip_prefix('(')
                            .and_then(|s| s.strip_suffix(')'))
                            .unwrap_or(elem.as_str());
                        for ch in body.chars() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    arity += 1;
                                    any = true;
                                }
                                c if !c.is_whitespace() => any = true,
                                _ => {}
                            }
                        }
                        if any {
                            arity += 1;
                        }
                        // Pass through as 10+arity so after +10 becomes 20+arity.
                        10 + arity.max(1)
                    } else {
                        0
                    }
                } else {
                    0
                };
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let fn_ty = i8_ptr_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                        BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                );
                let callee = self
                    .module
                    .get_function("mimi_list_result_map_to_json")
                    .unwrap_or_else(|| {
                        self.module.add_function(
                            "mimi_list_result_map_to_json",
                            fn_ty,
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                let raw = self
                    .build_call(
                        callee,
                        &[
                            BasicMetadataValueEnum::PointerValue(alloca),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "to_json_list_res_map",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("list result map to_json void")?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            } else if inner.starts_with("Result") {
                let ok_inner = inner
                    .strip_prefix("Result<")
                    .and_then(|s| {
                        let mut depth = 0i32;
                        for (i, ch) in s.char_indices() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    return Some(s[..i].trim());
                                }
                                _ => {}
                            }
                        }
                        None
                    })
                    .unwrap_or("");
                // Product-tuple / named-record / nested Result Ok — not bare scalar.
                // Product-tuple only (not named records — those use struct to_json).
                let ok_is_product = ok_inner.starts_with('(')
                    || ok_inner.contains("Tuple")
                    || self.is_product_tuple_alias(ok_inner);
                let ok_is_named_record = self
                    .type_defs
                    .get(ok_inner)
                    .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)));
                let ok_is_option_product = ok_inner.starts_with("Option")
                    && (ok_inner.contains('(')
                        || ok_inner.contains("Tuple")
                        || ok_inner.contains("Result"));
                if ok_is_product {
                    // Prefer runtime uniform pack path (from_json list result product).
                    let elem = if self.is_product_tuple_alias(ok_inner) {
                        self.resolve_alias_type_name(ok_inner)
                    } else {
                        ok_inner.to_string()
                    };
                    let raw = self.emit_list_result_product_runtime(alloca, &elem, 0)?;
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                if ok_is_named_record {
                    let raw = self.emit_list_result_product_to_json(alloca, inner)?;
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                if ok_inner.starts_with("Map<string, ") {
                    if let Some(inner_val) = ok_inner
                        .strip_prefix("Map<string, ")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if inner_val.starts_with('(') || self.is_product_tuple_alias(inner_val) {
                            let elem = if self.is_product_tuple_alias(inner_val) {
                                self.resolve_alias_type_name(inner_val)
                            } else {
                                inner_val.to_string()
                            };
                            let raw =
                                self.emit_list_result_map_product_runtime(alloca, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                    }
                }
                if ok_is_option_product {
                    let raw = self.emit_list_result_option_product_to_json(alloca, inner)?;
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                // Nested Result Ok: Result<Result<…>, E> — full LLVM load + recurse.
                if ok_inner.starts_with("Result") {
                    let raw = self.emit_list_result_product_to_json(alloca, inner)?;
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                // Scalar Result Ok — runtime i64 helper.
                "mimi_list_result_i64_to_json"
            } else if inner.starts_with('(') || self.is_product_tuple_alias(inner) {
                // List of product tuples (or type-alias of them).
                let elem = if self.is_product_tuple_alias(inner) {
                    self.resolve_alias_type_name(inner)
                } else {
                    inner.to_string()
                };
                let raw = self.emit_list_product_tuple_to_json(alloca, &elem)?;
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            } else {
                match inner {
                    "string" => "mimi_list_str_to_json",
                    "f64" | "f32" => "mimi_list_f64_to_json",
                    "bool" => "mimi_list_bool_to_json",
                    _ => "mimi_list_i64_to_json",
                }
            };
            let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let fn_ty = i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
            let callee = self.module.get_function(rt_fn_name).unwrap_or_else(|| {
                self.module.add_function(
                    rt_fn_name,
                    fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
            let raw = self
                .build_call(
                    callee,
                    &[BasicMetadataValueEnum::PointerValue(alloca)],
                    "to_json_list",
                )?
                .try_as_basic_value_opt()
                .ok_or("to_json list helper returned void")?
                .into_pointer_value();
            self.register_heap_alloc(raw);
            return Ok(Some(self.wrap_c_string(raw)?));
        }
        // Map / Map<string, …> → typed map JSON helpers.
        // 0.39.136 (L1): the checker's canonical name for a dynamic
        // `map_new()` map is `Record` — accept it here too, or the
        // untyped-map handle falls through to compile_to_json's
        // integer arm and to_json prints the raw handle natively
        // while the VM serializes real JSON.
        if obj_type == "Map" || obj_type.starts_with("Map<") || obj_type == "Record" {
            let handle = match &arg0 {
                BasicMetadataValueEnum::IntValue(iv) => *iv,
                BasicMetadataValueEnum::PointerValue(_) => {
                    return Err(CompileError::Generic(
                        "to_json: Map handle must be i64".into(),
                    ));
                }
                other => {
                    return Err(CompileError::Generic(format!(
                        "to_json: unexpected Map argument kind {:?}",
                        other
                    )))
                }
            };
            if let Some(val_ty) = obj_type
                .strip_prefix("Map<string, ")
                .and_then(|s| s.strip_suffix('>'))
            {
                if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                    let elem = if self.is_product_tuple_alias(val_ty) {
                        self.resolve_alias_type_name(val_ty)
                    } else {
                        val_ty.to_string()
                    };
                    let raw = self.emit_map_product_to_json(handle, &elem, 0)?;
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                if let Some(list_elem) = val_ty
                    .strip_prefix("List<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem) {
                        let elem = if self.is_product_tuple_alias(list_elem) {
                            self.resolve_alias_type_name(list_elem)
                        } else {
                            list_elem.to_string()
                        };
                        let raw = self.emit_map_list_product_to_json(handle, &elem, 0)?;
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                    if list_elem.starts_with("Map<string, ") {
                        if let Some(map_val) = list_elem
                            .strip_prefix("Map<string, ")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if map_val.starts_with('(') || self.is_product_tuple_alias(map_val) {
                                let elem = if self.is_product_tuple_alias(map_val) {
                                    self.resolve_alias_type_name(map_val)
                                } else {
                                    map_val.to_string()
                                };
                                let raw =
                                    self.emit_map_list_map_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                            if let Some(list_elem2) = map_val
                                .strip_prefix("List<")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if list_elem2.starts_with('(')
                                    || self.is_product_tuple_alias(list_elem2)
                                {
                                    let elem = if self.is_product_tuple_alias(list_elem2) {
                                        self.resolve_alias_type_name(list_elem2)
                                    } else {
                                        list_elem2.to_string()
                                    };
                                    let raw = self
                                        .emit_map_list_map_list_product_to_json(handle, &elem, 0)?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                    if let Some(set_elem) = list_elem
                        .strip_prefix("Set<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                            let elem = if self.is_product_tuple_alias(set_elem) {
                                self.resolve_alias_type_name(set_elem)
                            } else {
                                set_elem.to_string()
                            };
                            let raw = self.emit_map_list_set_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        if set_elem.starts_with("Map<string, ") {
                            if let Some(val_ty) = set_elem
                                .strip_prefix("Map<string, ")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                    let elem = if self.is_product_tuple_alias(val_ty) {
                                        self.resolve_alias_type_name(val_ty)
                                    } else {
                                        val_ty.to_string()
                                    };
                                    let raw = self
                                        .emit_map_list_set_map_product_to_json(handle, &elem, 0)?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                        if let Some(opt_inner) = set_elem
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner)
                            {
                                let elem = if self.is_product_tuple_alias(opt_inner) {
                                    self.resolve_alias_type_name(opt_inner)
                                } else {
                                    opt_inner.to_string()
                                };
                                let raw = self
                                    .emit_map_list_set_option_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                        if set_elem.starts_with("Result<") {
                            if let Some(ok_ty) = set_elem.strip_prefix("Result<").and_then(|s| {
                                let mut depth = 0i32;
                                for (i, ch) in s.char_indices() {
                                    match ch {
                                        '<' | '(' => depth += 1,
                                        '>' | ')' => depth -= 1,
                                        ',' if depth == 0 => {
                                            return Some(s[..i].trim());
                                        }
                                        _ => {}
                                    }
                                }
                                None
                            }) {
                                if ok_ty.starts_with('(') || self.is_product_tuple_alias(ok_ty) {
                                    let elem = if self.is_product_tuple_alias(ok_ty) {
                                        self.resolve_alias_type_name(ok_ty)
                                    } else {
                                        ok_ty.to_string()
                                    };
                                    let raw = self.emit_map_list_set_result_product_to_json(
                                        handle, &elem, 0,
                                    )?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                    if let Some(opt_inner) = list_elem
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner) {
                            let elem = if self.is_product_tuple_alias(opt_inner) {
                                self.resolve_alias_type_name(opt_inner)
                            } else {
                                opt_inner.to_string()
                            };
                            let raw =
                                self.emit_map_list_option_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        if let Some(set_elem) = opt_inner
                            .strip_prefix("Set<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                                let elem = if self.is_product_tuple_alias(set_elem) {
                                    self.resolve_alias_type_name(set_elem)
                                } else {
                                    set_elem.to_string()
                                };
                                let raw = self
                                    .emit_map_list_option_set_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                    }
                    if let Some(res_ok) = list_elem
                        .strip_prefix("Result<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        let product = if res_ok.starts_with('(') {
                            let mut depth = 0i32;
                            let mut end = 0usize;
                            for (i, ch) in res_ok.char_indices() {
                                match ch {
                                    '(' => depth += 1,
                                    ')' => {
                                        depth -= 1;
                                        if depth == 0 {
                                            end = i + 1;
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            res_ok[..end].to_string()
                        } else if let Some(c) = res_ok.find(',') {
                            res_ok[..c].to_string()
                        } else {
                            res_ok.to_string()
                        };
                        if product.starts_with('(') || self.is_product_tuple_alias(&product) {
                            let elem = if self.is_product_tuple_alias(&product) {
                                self.resolve_alias_type_name(&product)
                            } else {
                                product
                            };
                            let raw =
                                self.emit_map_list_result_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        let res_first = {
                            let mut depth = 0i32;
                            let mut end = res_ok.len();
                            for (i, ch) in res_ok.char_indices() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        end = i;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            res_ok[..end].trim().to_string()
                        };
                        if let Some(opt_inner) = res_first
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner)
                            {
                                let elem = if self.is_product_tuple_alias(opt_inner) {
                                    self.resolve_alias_type_name(opt_inner)
                                } else {
                                    opt_inner.to_string()
                                };
                                let raw = self.emit_map_list_result_option_product_to_json(
                                    handle, &elem, 0,
                                )?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                    }
                }
                if let Some(set_elem) = val_ty
                    .strip_prefix("Set<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                        let elem = if self.is_product_tuple_alias(set_elem) {
                            self.resolve_alias_type_name(set_elem)
                        } else {
                            set_elem.to_string()
                        };
                        let raw = self.emit_map_set_product_to_json(handle, &elem, 0)?;
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                    if let Some(list_elem) = set_elem
                        .strip_prefix("List<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem) {
                            let elem = if self.is_product_tuple_alias(list_elem) {
                                self.resolve_alias_type_name(list_elem)
                            } else {
                                list_elem.to_string()
                            };
                            let raw = self.emit_map_set_list_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        if list_elem.starts_with("Map<string, ") {
                            if let Some(val_ty) = list_elem
                                .strip_prefix("Map<string, ")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                    let elem = if self.is_product_tuple_alias(val_ty) {
                                        self.resolve_alias_type_name(val_ty)
                                    } else {
                                        val_ty.to_string()
                                    };
                                    let raw = self
                                        .emit_map_set_list_map_product_to_json(handle, &elem, 0)?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                    if set_elem.starts_with("Map<string, ") {
                        if let Some(val_ty) = set_elem
                            .strip_prefix("Map<string, ")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                let elem = if self.is_product_tuple_alias(val_ty) {
                                    self.resolve_alias_type_name(val_ty)
                                } else {
                                    val_ty.to_string()
                                };
                                let raw =
                                    self.emit_map_set_map_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                            if let Some(list_elem) = val_ty
                                .strip_prefix("List<")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if list_elem.starts_with('(')
                                    || self.is_product_tuple_alias(list_elem)
                                {
                                    let elem = if self.is_product_tuple_alias(list_elem) {
                                        self.resolve_alias_type_name(list_elem)
                                    } else {
                                        list_elem.to_string()
                                    };
                                    let raw = self
                                        .emit_map_set_map_list_product_to_json(handle, &elem, 0)?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                    if let Some(opt_inner) = set_elem
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner) {
                            let elem = if self.is_product_tuple_alias(opt_inner) {
                                self.resolve_alias_type_name(opt_inner)
                            } else {
                                opt_inner.to_string()
                            };
                            let raw = self.emit_map_set_option_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                    }
                    if let Some(res_ok) = set_elem
                        .strip_prefix("Result<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        let product = if res_ok.starts_with('(') {
                            let mut depth = 0i32;
                            let mut end = 0usize;
                            for (i, ch) in res_ok.char_indices() {
                                match ch {
                                    '(' => depth += 1,
                                    ')' => {
                                        depth -= 1;
                                        if depth == 0 {
                                            end = i + 1;
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            res_ok[..end].to_string()
                        } else if let Some(c) = res_ok.find(',') {
                            res_ok[..c].to_string()
                        } else {
                            res_ok.to_string()
                        };
                        if product.starts_with('(') || self.is_product_tuple_alias(&product) {
                            let elem = if self.is_product_tuple_alias(&product) {
                                self.resolve_alias_type_name(&product)
                            } else {
                                product
                            };
                            let raw = self.emit_map_set_result_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        let res_first = {
                            let mut depth = 0i32;
                            let mut end = res_ok.len();
                            for (i, ch) in res_ok.char_indices() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        end = i;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            res_ok[..end].trim().to_string()
                        };
                        if let Some(opt_inner) = res_first
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner)
                            {
                                let elem = if self.is_product_tuple_alias(opt_inner) {
                                    self.resolve_alias_type_name(opt_inner)
                                } else {
                                    opt_inner.to_string()
                                };
                                let raw = self
                                    .emit_map_set_result_option_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                    }
                }
                if let Some(opt_elem) = val_ty
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if opt_elem.starts_with('(') || self.is_product_tuple_alias(opt_elem) {
                        let elem = if self.is_product_tuple_alias(opt_elem) {
                            self.resolve_alias_type_name(opt_elem)
                        } else {
                            opt_elem.to_string()
                        };
                        let raw = self.emit_map_option_product_to_json(handle, &elem, 0)?;
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                    if opt_elem.starts_with("Map<string, ") {
                        if let Some(inner_val) = opt_elem
                            .strip_prefix("Map<string, ")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if inner_val.starts_with('(') || self.is_product_tuple_alias(inner_val)
                            {
                                let elem = if self.is_product_tuple_alias(inner_val) {
                                    self.resolve_alias_type_name(inner_val)
                                } else {
                                    inner_val.to_string()
                                };
                                let raw =
                                    self.emit_map_option_map_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                            if let Some(list_elem) = inner_val
                                .strip_prefix("List<")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if list_elem.starts_with('(')
                                    || self.is_product_tuple_alias(list_elem)
                                {
                                    let elem = if self.is_product_tuple_alias(list_elem) {
                                        self.resolve_alias_type_name(list_elem)
                                    } else {
                                        list_elem.to_string()
                                    };
                                    let raw = self.emit_map_option_map_list_product_to_json(
                                        handle, &elem, 0,
                                    )?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                    if let Some(set_elem) = opt_elem
                        .strip_prefix("Set<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                            let elem = if self.is_product_tuple_alias(set_elem) {
                                self.resolve_alias_type_name(set_elem)
                            } else {
                                set_elem.to_string()
                            };
                            let raw = self.emit_map_option_set_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        if let Some(list_elem) = set_elem
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem)
                            {
                                let elem = if self.is_product_tuple_alias(list_elem) {
                                    self.resolve_alias_type_name(list_elem)
                                } else {
                                    list_elem.to_string()
                                };
                                let raw = self
                                    .emit_map_option_set_list_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                        if set_elem.starts_with("Map<string, ") {
                            if let Some(val_ty) = set_elem
                                .strip_prefix("Map<string, ")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                    let elem = if self.is_product_tuple_alias(val_ty) {
                                        self.resolve_alias_type_name(val_ty)
                                    } else {
                                        val_ty.to_string()
                                    };
                                    let raw = self.emit_map_option_set_map_product_to_json(
                                        handle, &elem, 0,
                                    )?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                    if let Some(list_elem) = opt_elem
                        .strip_prefix("List<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem) {
                            let elem = if self.is_product_tuple_alias(list_elem) {
                                self.resolve_alias_type_name(list_elem)
                            } else {
                                list_elem.to_string()
                            };
                            let raw =
                                self.emit_map_option_list_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        if list_elem.starts_with("Map<string, ") {
                            if let Some(val_ty) = list_elem
                                .strip_prefix("Map<string, ")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                    let elem = if self.is_product_tuple_alias(val_ty) {
                                        self.resolve_alias_type_name(val_ty)
                                    } else {
                                        val_ty.to_string()
                                    };
                                    let raw = self.emit_map_option_list_map_product_to_json(
                                        handle, &elem, 0,
                                    )?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                    if let Some(res_ok) = opt_elem
                        .strip_prefix("Result<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        let product = if res_ok.starts_with('(') {
                            let mut depth = 0i32;
                            let mut end = 0usize;
                            for (i, ch) in res_ok.char_indices() {
                                match ch {
                                    '(' => depth += 1,
                                    ')' => {
                                        depth -= 1;
                                        if depth == 0 {
                                            end = i + 1;
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            res_ok[..end].to_string()
                        } else if let Some(c) = res_ok.find(',') {
                            res_ok[..c].to_string()
                        } else {
                            res_ok.to_string()
                        };
                        if product.starts_with('(') || self.is_product_tuple_alias(&product) {
                            let elem = if self.is_product_tuple_alias(&product) {
                                self.resolve_alias_type_name(&product)
                            } else {
                                product
                            };
                            let raw =
                                self.emit_map_option_result_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        // res_ok may be "List<(…), string" if strip_suffix only removed one >.
                        let res_first = {
                            let mut depth = 0i32;
                            let mut end = res_ok.len();
                            for (i, ch) in res_ok.char_indices() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        end = i;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            res_ok[..end].trim()
                        };
                        if let Some(list_elem) = res_first
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem)
                            {
                                let elem = if self.is_product_tuple_alias(list_elem) {
                                    self.resolve_alias_type_name(list_elem)
                                } else {
                                    list_elem.to_string()
                                };
                                let raw = self.emit_map_option_result_list_product_to_json(
                                    handle, &elem, 0,
                                )?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                    }
                }
                if val_ty.starts_with("Result<") {
                    if let Some(ok_ty) = val_ty.strip_prefix("Result<").and_then(|s| {
                        let mut depth = 0i32;
                        for (i, ch) in s.char_indices() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    return Some(s[..i].trim());
                                }
                                _ => {}
                            }
                        }
                        None
                    }) {
                        if ok_ty.starts_with('(') || self.is_product_tuple_alias(ok_ty) {
                            let elem = if self.is_product_tuple_alias(ok_ty) {
                                self.resolve_alias_type_name(ok_ty)
                            } else {
                                ok_ty.to_string()
                            };
                            let raw = self.emit_map_result_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        if ok_ty.starts_with("Map<string, ") {
                            if let Some(inner_val) = ok_ty
                                .strip_prefix("Map<string, ")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if inner_val.starts_with('(')
                                    || self.is_product_tuple_alias(inner_val)
                                {
                                    let elem = if self.is_product_tuple_alias(inner_val) {
                                        self.resolve_alias_type_name(inner_val)
                                    } else {
                                        inner_val.to_string()
                                    };
                                    let raw =
                                        self.emit_map_result_map_product_to_json(handle, &elem, 0)?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                        if let Some(set_elem) =
                            ok_ty.strip_prefix("Set<").and_then(|s| s.strip_suffix('>'))
                        {
                            if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                                let elem = if self.is_product_tuple_alias(set_elem) {
                                    self.resolve_alias_type_name(set_elem)
                                } else {
                                    set_elem.to_string()
                                };
                                let raw =
                                    self.emit_map_result_set_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                            if let Some(list_elem) = set_elem
                                .strip_prefix("List<")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if list_elem.starts_with('(')
                                    || self.is_product_tuple_alias(list_elem)
                                {
                                    let elem = if self.is_product_tuple_alias(list_elem) {
                                        self.resolve_alias_type_name(list_elem)
                                    } else {
                                        list_elem.to_string()
                                    };
                                    let raw = self.emit_map_result_set_list_product_to_json(
                                        handle, &elem, 0,
                                    )?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                            if set_elem.starts_with("Map<string, ") {
                                if let Some(val_ty) = set_elem
                                    .strip_prefix("Map<string, ")
                                    .and_then(|s| s.strip_suffix('>'))
                                {
                                    if val_ty.starts_with('(')
                                        || self.is_product_tuple_alias(val_ty)
                                    {
                                        let elem = if self.is_product_tuple_alias(val_ty) {
                                            self.resolve_alias_type_name(val_ty)
                                        } else {
                                            val_ty.to_string()
                                        };
                                        let raw = self.emit_map_result_set_map_product_to_json(
                                            handle, &elem, 0,
                                        )?;
                                        self.register_heap_alloc(raw);
                                        return Ok(Some(self.wrap_c_string(raw)?));
                                    }
                                }
                            }
                        }
                        if let Some(list_elem) = ok_ty
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem)
                            {
                                let elem = if self.is_product_tuple_alias(list_elem) {
                                    self.resolve_alias_type_name(list_elem)
                                } else {
                                    list_elem.to_string()
                                };
                                let raw =
                                    self.emit_map_result_list_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                            if list_elem.starts_with("Map<string, ") {
                                if let Some(val_ty) = list_elem
                                    .strip_prefix("Map<string, ")
                                    .and_then(|s| s.strip_suffix('>'))
                                {
                                    if val_ty.starts_with('(')
                                        || self.is_product_tuple_alias(val_ty)
                                    {
                                        let elem = if self.is_product_tuple_alias(val_ty) {
                                            self.resolve_alias_type_name(val_ty)
                                        } else {
                                            val_ty.to_string()
                                        };
                                        let raw = self.emit_map_result_list_map_product_to_json(
                                            handle, &elem, 0,
                                        )?;
                                        self.register_heap_alloc(raw);
                                        return Ok(Some(self.wrap_c_string(raw)?));
                                    }
                                }
                            }
                            if let Some(set_elem) = list_elem
                                .strip_prefix("Set<")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if set_elem.starts_with('(')
                                    || self.is_product_tuple_alias(set_elem)
                                {
                                    let elem = if self.is_product_tuple_alias(set_elem) {
                                        self.resolve_alias_type_name(set_elem)
                                    } else {
                                        set_elem.to_string()
                                    };
                                    let raw = self.emit_map_result_list_set_product_to_json(
                                        handle, &elem, 0,
                                    )?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                            if let Some(opt_inner) = list_elem
                                .strip_prefix("Option<")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if opt_inner.starts_with('(')
                                    || self.is_product_tuple_alias(opt_inner)
                                {
                                    let elem = if self.is_product_tuple_alias(opt_inner) {
                                        self.resolve_alias_type_name(opt_inner)
                                    } else {
                                        opt_inner.to_string()
                                    };
                                    let raw = self.emit_map_result_list_option_product_to_json(
                                        handle, &elem, 0,
                                    )?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                        if let Some(opt_elem) = ok_ty
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if opt_elem.starts_with('(') || self.is_product_tuple_alias(opt_elem) {
                                let elem = if self.is_product_tuple_alias(opt_elem) {
                                    self.resolve_alias_type_name(opt_elem)
                                } else {
                                    opt_elem.to_string()
                                };
                                let raw =
                                    self.emit_map_result_option_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                            if let Some(list_elem) = opt_elem
                                .strip_prefix("List<")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if list_elem.starts_with('(')
                                    || self.is_product_tuple_alias(list_elem)
                                {
                                    let elem = if self.is_product_tuple_alias(list_elem) {
                                        self.resolve_alias_type_name(list_elem)
                                    } else {
                                        list_elem.to_string()
                                    };
                                    let raw = self.emit_map_result_option_list_product_to_json(
                                        handle, &elem, 0,
                                    )?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                }
                if val_ty.starts_with("Map<string, ") {
                    if let Some(inner_val) = val_ty
                        .strip_prefix("Map<string, ")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if inner_val.starts_with('(') || self.is_product_tuple_alias(inner_val) {
                            let elem = if self.is_product_tuple_alias(inner_val) {
                                self.resolve_alias_type_name(inner_val)
                            } else {
                                inner_val.to_string()
                            };
                            let raw = self.emit_map_map_product_to_json(handle, &elem, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        if let Some(set_elem) = inner_val
                            .strip_prefix("Set<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                                let elem = if self.is_product_tuple_alias(set_elem) {
                                    self.resolve_alias_type_name(set_elem)
                                } else {
                                    set_elem.to_string()
                                };
                                let raw =
                                    self.emit_map_map_set_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                        if let Some(list_elem) = inner_val
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem)
                            {
                                let elem = if self.is_product_tuple_alias(list_elem) {
                                    self.resolve_alias_type_name(list_elem)
                                } else {
                                    list_elem.to_string()
                                };
                                let raw =
                                    self.emit_map_map_list_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                        if let Some(opt_inner) = inner_val
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner)
                            {
                                let elem = if self.is_product_tuple_alias(opt_inner) {
                                    self.resolve_alias_type_name(opt_inner)
                                } else {
                                    opt_inner.to_string()
                                };
                                let raw =
                                    self.emit_map_map_option_product_to_json(handle, &elem, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                        if inner_val.starts_with("Result<") {
                            if let Some(ok_ty) = inner_val.strip_prefix("Result<").and_then(|s| {
                                let mut depth = 0i32;
                                for (i, ch) in s.char_indices() {
                                    match ch {
                                        '<' | '(' => depth += 1,
                                        '>' | ')' => depth -= 1,
                                        ',' if depth == 0 => {
                                            return Some(s[..i].trim());
                                        }
                                        _ => {}
                                    }
                                }
                                None
                            }) {
                                if ok_ty.starts_with('(') || self.is_product_tuple_alias(ok_ty) {
                                    let elem = if self.is_product_tuple_alias(ok_ty) {
                                        self.resolve_alias_type_name(ok_ty)
                                    } else {
                                        ok_ty.to_string()
                                    };
                                    let raw =
                                        self.emit_map_map_result_product_to_json(handle, &elem, 0)?;
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                }
            }
            let fn_name = if obj_type.contains("Map<string, string>") {
                "mimi_map_to_json_string"
            } else if obj_type.contains("Map<string, bool>") {
                "mimi_map_to_json_bool"
            } else if obj_type.contains("Map<string, f64>") || obj_type.contains("Map<string, f32>")
            {
                "mimi_map_to_json_f64_serde"
            } else if obj_type == "Record" || obj_type == "Map" {
                // Untyped `map_new()` map: values are Any handles —
                // render strings as JSON strings, ints bare (VM
                // parity). Typed Map<…> modes keep their exact paths.
                "mimi_map_to_json_any"
            } else {
                "mimi_map_to_json_i64"
            };
            let func = self.get_runtime_fn(fn_name)?;
            let raw = self
                .build_call(
                    func,
                    &[BasicMetadataValueEnum::IntValue(handle)],
                    "to_json_map",
                )?
                .try_as_basic_value_opt()
                .ok_or("map to_json returned void")?
                .into_pointer_value();
            self.register_heap_alloc(raw);
            return Ok(Some(self.wrap_c_string(raw)?));
        }
        // Set / Set<…> → typed set JSON helpers
        if obj_type == "Set" || obj_type.starts_with("Set<") || obj_type == "set" {
            let handle = match &arg0 {
                BasicMetadataValueEnum::IntValue(iv) => *iv,
                BasicMetadataValueEnum::PointerValue(_) => {
                    return Err(CompileError::Generic(
                        "to_json: Set handle must be i64".into(),
                    ));
                }
                other => {
                    return Err(CompileError::Generic(format!(
                        "to_json: unexpected Set argument kind {:?}",
                        other
                    )))
                }
            };
            if let Some(elem) = obj_type
                .strip_prefix("Set<")
                .and_then(|s| s.strip_suffix('>'))
            {
                if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                    let resolved = if self.is_product_tuple_alias(elem) {
                        self.resolve_alias_type_name(elem)
                    } else {
                        elem.to_string()
                    };
                    let raw = self.emit_set_product_to_json(handle, &resolved, 0)?;
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                if elem.starts_with("Map<string, ") {
                    if let Some(val_ty) = elem
                        .strip_prefix("Map<string, ")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                            let resolved = if self.is_product_tuple_alias(val_ty) {
                                self.resolve_alias_type_name(val_ty)
                            } else {
                                val_ty.to_string()
                            };
                            let arity = {
                                let body = resolved
                                    .strip_prefix('(')
                                    .and_then(|s| s.strip_suffix(')'))
                                    .unwrap_or(&resolved);
                                let mut arity = 0i64;
                                let mut depth = 0i32;
                                let mut any = false;
                                for ch in body.chars() {
                                    match ch {
                                        '<' | '(' => depth += 1,
                                        '>' | ')' => depth -= 1,
                                        ',' if depth == 0 => {
                                            arity += 1;
                                            any = true;
                                        }
                                        c if !c.is_whitespace() => any = true,
                                        _ => {}
                                    }
                                }
                                if any {
                                    arity += 1;
                                }
                                arity.max(1)
                            };
                            let func = self.get_runtime_fn("mimi_set_to_json_map_product_i64")?;
                            let i64_ty = self.context.i64_type();
                            let raw = self
                                .build_call(
                                    func,
                                    &[
                                        BasicMetadataValueEnum::IntValue(handle),
                                        BasicMetadataValueEnum::IntValue(
                                            i64_ty.const_int(arity as u64, false),
                                        ),
                                        BasicMetadataValueEnum::IntValue(
                                            i64_ty.const_int(0, false),
                                        ),
                                    ],
                                    "set_map_product_json",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("set map product to_json void")?
                                .into_pointer_value();
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        if let Some(list_elem) = val_ty
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem)
                            {
                                let resolved = if self.is_product_tuple_alias(list_elem) {
                                    self.resolve_alias_type_name(list_elem)
                                } else {
                                    list_elem.to_string()
                                };
                                let arity = {
                                    let body = resolved
                                        .strip_prefix('(')
                                        .and_then(|s| s.strip_suffix(')'))
                                        .unwrap_or(&resolved);
                                    let mut arity = 0i64;
                                    let mut depth = 0i32;
                                    let mut any = false;
                                    for ch in body.chars() {
                                        match ch {
                                            '<' | '(' => depth += 1,
                                            '>' | ')' => depth -= 1,
                                            ',' if depth == 0 => {
                                                arity += 1;
                                                any = true;
                                            }
                                            c if !c.is_whitespace() => any = true,
                                            _ => {}
                                        }
                                    }
                                    if any {
                                        arity += 1;
                                    }
                                    arity.max(1)
                                };
                                let func =
                                    self.get_runtime_fn("mimi_set_to_json_map_list_product_i64")?;
                                let i64_ty = self.context.i64_type();
                                let raw = self
                                    .build_call(
                                        func,
                                        &[
                                            BasicMetadataValueEnum::IntValue(handle),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(arity as u64, false),
                                            ),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(0, false),
                                            ),
                                        ],
                                        "set_map_list_product_json",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("set map list product to_json void")?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                        if let Some(set_elem) = val_ty
                            .strip_prefix("Set<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                                let resolved = if self.is_product_tuple_alias(set_elem) {
                                    self.resolve_alias_type_name(set_elem)
                                } else {
                                    set_elem.to_string()
                                };
                                let arity = {
                                    let body = resolved
                                        .strip_prefix('(')
                                        .and_then(|s| s.strip_suffix(')'))
                                        .unwrap_or(&resolved);
                                    let mut arity = 0i64;
                                    let mut depth = 0i32;
                                    let mut any = false;
                                    for ch in body.chars() {
                                        match ch {
                                            '<' | '(' => depth += 1,
                                            '>' | ')' => depth -= 1,
                                            ',' if depth == 0 => {
                                                arity += 1;
                                                any = true;
                                            }
                                            c if !c.is_whitespace() => any = true,
                                            _ => {}
                                        }
                                    }
                                    if any {
                                        arity += 1;
                                    }
                                    arity.max(1)
                                };
                                let func =
                                    self.get_runtime_fn("mimi_set_to_json_map_set_product_i64")?;
                                let i64_ty = self.context.i64_type();
                                let raw = self
                                    .build_call(
                                        func,
                                        &[
                                            BasicMetadataValueEnum::IntValue(handle),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(arity as u64, false),
                                            ),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(0, false),
                                            ),
                                        ],
                                        "set_map_set_product_json",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("set map set product to_json void")?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                    }
                }
                if let Some(opt_inner) = elem
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if opt_inner.starts_with("Map<string, ") {
                        if let Some(val_ty) = opt_inner
                            .strip_prefix("Map<string, ")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                let resolved = if self.is_product_tuple_alias(val_ty) {
                                    self.resolve_alias_type_name(val_ty)
                                } else {
                                    val_ty.to_string()
                                };
                                let arity = {
                                    let body = resolved
                                        .strip_prefix('(')
                                        .and_then(|s| s.strip_suffix(')'))
                                        .unwrap_or(&resolved);
                                    let mut arity = 0i64;
                                    let mut depth = 0i32;
                                    let mut any = false;
                                    for ch in body.chars() {
                                        match ch {
                                            '<' | '(' => depth += 1,
                                            '>' | ')' => depth -= 1,
                                            ',' if depth == 0 => {
                                                arity += 1;
                                                any = true;
                                            }
                                            c if !c.is_whitespace() => any = true,
                                            _ => {}
                                        }
                                    }
                                    if any {
                                        arity += 1;
                                    }
                                    arity.max(1)
                                };
                                let func =
                                    self.get_runtime_fn("mimi_set_to_json_option_map_product_i64")?;
                                let i64_ty = self.context.i64_type();
                                let raw = self
                                    .build_call(
                                        func,
                                        &[
                                            BasicMetadataValueEnum::IntValue(handle),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(arity as u64, false),
                                            ),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(0, false),
                                            ),
                                        ],
                                        "set_option_map_product_json",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("set option map product to_json void")?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                    }
                }
                if let Some(res_ok) = elem.strip_prefix("Result<").and_then(|s| {
                    let mut depth = 0i32;
                    for (i, ch) in s.char_indices() {
                        match ch {
                            '<' | '(' => depth += 1,
                            '>' | ')' => depth -= 1,
                            ',' if depth == 0 => {
                                return Some(s[..i].trim());
                            }
                            _ => {}
                        }
                    }
                    None
                }) {
                    if res_ok.starts_with("Map<string, ") {
                        if let Some(val_ty) = res_ok
                            .strip_prefix("Map<string, ")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                let resolved = if self.is_product_tuple_alias(val_ty) {
                                    self.resolve_alias_type_name(val_ty)
                                } else {
                                    val_ty.to_string()
                                };
                                let arity = {
                                    let body = resolved
                                        .strip_prefix('(')
                                        .and_then(|s| s.strip_suffix(')'))
                                        .unwrap_or(&resolved);
                                    let mut arity = 0i64;
                                    let mut depth = 0i32;
                                    let mut any = false;
                                    for ch in body.chars() {
                                        match ch {
                                            '<' | '(' => depth += 1,
                                            '>' | ')' => depth -= 1,
                                            ',' if depth == 0 => {
                                                arity += 1;
                                                any = true;
                                            }
                                            c if !c.is_whitespace() => any = true,
                                            _ => {}
                                        }
                                    }
                                    if any {
                                        arity += 1;
                                    }
                                    arity.max(1)
                                };
                                let func =
                                    self.get_runtime_fn("mimi_set_to_json_result_map_product_i64")?;
                                let i64_ty = self.context.i64_type();
                                let raw = self
                                    .build_call(
                                        func,
                                        &[
                                            BasicMetadataValueEnum::IntValue(handle),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(arity as u64, false),
                                            ),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(0, false),
                                            ),
                                        ],
                                        "set_result_map_product_json",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("set result map product to_json void")?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                        if let Some(list_elem) = res_ok
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem)
                            {
                                let resolved = if self.is_product_tuple_alias(list_elem) {
                                    self.resolve_alias_type_name(list_elem)
                                } else {
                                    list_elem.to_string()
                                };
                                let arity = {
                                    let body = resolved
                                        .strip_prefix('(')
                                        .and_then(|s| s.strip_suffix(')'))
                                        .unwrap_or(&resolved);
                                    let mut arity = 0i64;
                                    let mut depth = 0i32;
                                    let mut any = false;
                                    for ch in body.chars() {
                                        match ch {
                                            '<' | '(' => depth += 1,
                                            '>' | ')' => depth -= 1,
                                            ',' if depth == 0 => {
                                                arity += 1;
                                                any = true;
                                            }
                                            c if !c.is_whitespace() => any = true,
                                            _ => {}
                                        }
                                    }
                                    if any {
                                        arity += 1;
                                    }
                                    arity.max(1)
                                };
                                let func = self
                                    .get_runtime_fn("mimi_set_to_json_result_list_product_i64")?;
                                let i64_ty = self.context.i64_type();
                                let raw = self
                                    .build_call(
                                        func,
                                        &[
                                            BasicMetadataValueEnum::IntValue(handle),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(arity as u64, false),
                                            ),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(0, false),
                                            ),
                                        ],
                                        "set_result_list_product_json",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("set result list product to_json void")?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                    }
                }
                if let Some(list_elem) =
                    elem.strip_prefix("List<").and_then(|s| s.strip_suffix('>'))
                {
                    if list_elem.starts_with("Map<string, ") {
                        if let Some(val_ty) = list_elem
                            .strip_prefix("Map<string, ")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                let resolved = if self.is_product_tuple_alias(val_ty) {
                                    self.resolve_alias_type_name(val_ty)
                                } else {
                                    val_ty.to_string()
                                };
                                let arity = {
                                    let body = resolved
                                        .strip_prefix('(')
                                        .and_then(|s| s.strip_suffix(')'))
                                        .unwrap_or(&resolved);
                                    let mut arity = 0i64;
                                    let mut depth = 0i32;
                                    let mut any = false;
                                    for ch in body.chars() {
                                        match ch {
                                            '<' | '(' => depth += 1,
                                            '>' | ')' => depth -= 1,
                                            ',' if depth == 0 => {
                                                arity += 1;
                                                any = true;
                                            }
                                            c if !c.is_whitespace() => any = true,
                                            _ => {}
                                        }
                                    }
                                    if any {
                                        arity += 1;
                                    }
                                    arity.max(1)
                                };
                                let func =
                                    self.get_runtime_fn("mimi_set_to_json_list_map_product_i64")?;
                                let i64_ty = self.context.i64_type();
                                let raw = self
                                    .build_call(
                                        func,
                                        &[
                                            BasicMetadataValueEnum::IntValue(handle),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(arity as u64, false),
                                            ),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(0, false),
                                            ),
                                        ],
                                        "set_list_map_product_json",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("set list map product to_json void")?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                    }
                }
                if let Some(opt_elem) = elem
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if opt_elem.starts_with('(') || self.is_product_tuple_alias(opt_elem) {
                        let resolved = if self.is_product_tuple_alias(opt_elem) {
                            self.resolve_alias_type_name(opt_elem)
                        } else {
                            opt_elem.to_string()
                        };
                        let raw = self.emit_set_option_product_to_json(handle, &resolved, 0)?;
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                    if let Some(res_ok) = opt_elem.strip_prefix("Result<").and_then(|s| {
                        let mut depth = 0i32;
                        for (i, ch) in s.char_indices() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    return Some(s[..i].trim());
                                }
                                _ => {}
                            }
                        }
                        None
                    }) {
                        if res_ok.starts_with('(') || self.is_product_tuple_alias(res_ok) {
                            let resolved = if self.is_product_tuple_alias(res_ok) {
                                self.resolve_alias_type_name(res_ok)
                            } else {
                                res_ok.to_string()
                            };
                            let raw =
                                self.emit_set_option_result_product_to_json(handle, &resolved, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                    }
                }
                if elem.starts_with("Result<") {
                    if let Some(ok_ty) = elem.strip_prefix("Result<").and_then(|s| {
                        let mut depth = 0i32;
                        for (i, ch) in s.char_indices() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    return Some(s[..i].trim());
                                }
                                _ => {}
                            }
                        }
                        None
                    }) {
                        if ok_ty.starts_with('(') || self.is_product_tuple_alias(ok_ty) {
                            let resolved = if self.is_product_tuple_alias(ok_ty) {
                                self.resolve_alias_type_name(ok_ty)
                            } else {
                                ok_ty.to_string()
                            };
                            let raw = self.emit_set_result_product_to_json(handle, &resolved, 0)?;
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                        if let Some(opt_inner) = ok_ty
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner)
                            {
                                let resolved = if self.is_product_tuple_alias(opt_inner) {
                                    self.resolve_alias_type_name(opt_inner)
                                } else {
                                    opt_inner.to_string()
                                };
                                let raw = self
                                    .emit_set_result_option_product_to_json(handle, &resolved, 0)?;
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                        if let Some(list_elem) = ok_ty
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem)
                            {
                                let resolved = if self.is_product_tuple_alias(list_elem) {
                                    self.resolve_alias_type_name(list_elem)
                                } else {
                                    list_elem.to_string()
                                };
                                let arity = {
                                    let body = resolved
                                        .strip_prefix('(')
                                        .and_then(|s| s.strip_suffix(')'))
                                        .unwrap_or(&resolved);
                                    let mut arity = 0i64;
                                    let mut depth = 0i32;
                                    let mut any = false;
                                    for ch in body.chars() {
                                        match ch {
                                            '<' | '(' => depth += 1,
                                            '>' | ')' => depth -= 1,
                                            ',' if depth == 0 => {
                                                arity += 1;
                                                any = true;
                                            }
                                            c if !c.is_whitespace() => any = true,
                                            _ => {}
                                        }
                                    }
                                    if any {
                                        arity += 1;
                                    }
                                    arity.max(1)
                                };
                                let func = self
                                    .get_runtime_fn("mimi_set_to_json_result_list_product_i64")?;
                                let i64_ty = self.context.i64_type();
                                let raw = self
                                    .build_call(
                                        func,
                                        &[
                                            BasicMetadataValueEnum::IntValue(handle),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(arity as u64, false),
                                            ),
                                            BasicMetadataValueEnum::IntValue(
                                                i64_ty.const_int(0, false),
                                            ),
                                        ],
                                        "set_result_list_product_json2",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("set result list product to_json void")?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return Ok(Some(self.wrap_c_string(raw)?));
                            }
                        }
                        if ok_ty.starts_with("Map<string, ") {
                            if let Some(val_ty) = ok_ty
                                .strip_prefix("Map<string, ")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                    let resolved = if self.is_product_tuple_alias(val_ty) {
                                        self.resolve_alias_type_name(val_ty)
                                    } else {
                                        val_ty.to_string()
                                    };
                                    let arity = {
                                        let body = resolved
                                            .strip_prefix('(')
                                            .and_then(|s| s.strip_suffix(')'))
                                            .unwrap_or(&resolved);
                                        let mut arity = 0i64;
                                        let mut depth = 0i32;
                                        let mut any = false;
                                        for ch in body.chars() {
                                            match ch {
                                                '<' | '(' => depth += 1,
                                                '>' | ')' => depth -= 1,
                                                ',' if depth == 0 => {
                                                    arity += 1;
                                                    any = true;
                                                }
                                                c if !c.is_whitespace() => any = true,
                                                _ => {}
                                            }
                                        }
                                        if any {
                                            arity += 1;
                                        }
                                        arity.max(1)
                                    };
                                    let func = self.get_runtime_fn(
                                        "mimi_set_to_json_result_map_product_i64",
                                    )?;
                                    let i64_ty = self.context.i64_type();
                                    let raw = self
                                        .build_call(
                                            func,
                                            &[
                                                BasicMetadataValueEnum::IntValue(handle),
                                                BasicMetadataValueEnum::IntValue(
                                                    i64_ty.const_int(arity as u64, false),
                                                ),
                                                BasicMetadataValueEnum::IntValue(
                                                    i64_ty.const_int(0, false),
                                                ),
                                            ],
                                            "set_result_map_product_json2",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("set result map product to_json void")?
                                        .into_pointer_value();
                                    self.register_heap_alloc(raw);
                                    return Ok(Some(self.wrap_c_string(raw)?));
                                }
                            }
                        }
                    }
                }
            }
            let fn_name = if obj_type.contains("Set<string>") {
                "mimi_set_to_json_string"
            } else if obj_type.contains("Set<bool>") {
                "mimi_set_to_json_bool"
            } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>") {
                "mimi_set_to_json_f64"
            } else {
                "mimi_set_to_json_i64"
            };
            let func = self.get_runtime_fn(fn_name)?;
            let raw = self
                .build_call(
                    func,
                    &[BasicMetadataValueEnum::IntValue(handle)],
                    "to_json_set",
                )?
                .try_as_basic_value_opt()
                .ok_or("set to_json returned void")?
                .into_pointer_value();
            self.register_heap_alloc(raw);
            return Ok(Some(self.wrap_c_string(raw)?));
        }
        // Option / Option<T> with integer/handle payload: {i1,i64}
        // or by-value struct payload ({i1, tuple|record}).
        if obj_type == "Option"
            || obj_type.starts_with("Option<")
            || (arg_is_option_shape
                && !obj_type.starts_with("Result")
                && !self.type_defs.contains_key(&obj_type))
        {
            let opt_load_sty = {
                let parsed = crate::codegen::extract_list_elem_type(&format!("List<{}>", obj_type));
                // extract_list_elem_type("List<Option<P>>") → Option<P>
                let opt_ty = parsed.unwrap_or_else(|| {
                    crate::ast::Type::Name(
                        "Option".into(),
                        vec![crate::ast::Type::Name("i64".into(), vec![])],
                    )
                });
                match self.llvm_type_for(&opt_ty) {
                    Some(BasicTypeEnum::StructType(s)) => s,
                    _ => self.context.struct_type(
                        &[
                            self.context.bool_type().into(),
                            self.context.i64_type().into(),
                        ],
                        false,
                    ),
                }
            };
            let sv = match &arg0 {
                BasicMetadataValueEnum::StructValue(s) => *s,
                BasicMetadataValueEnum::PointerValue(pv) => {
                    let loaded = self
                        .builder
                        .build_load(BasicTypeEnum::StructType(opt_load_sty), *pv, "opt_load")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_struct_value();
                    loaded
                }
                other => {
                    return Err(CompileError::Generic(format!(
                        "to_json: unexpected Option argument kind {:?}",
                        other
                    )))
                }
            };
            let disc = self
                .build_extract_value(sv.into(), 0, "opt_disc")?
                .into_int_value();
            let disc_i64 = self
                .builder
                .build_int_z_extend(disc, self.context.i64_type(), "opt_disc_i64")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let payload_bv = self.build_extract_value(sv.into(), 1, "opt_payload")?;
            // D-3: heap-string payload is StructValue {ptr,i64} — must
            // NOT reach the generic mimi_option_i64_to_json scalar path
            // (which would print the pointer as a number) nor the
            // product-tuple path ([ptr,len]). Made `mut` so the
            // record/product-tuple payload branch below can also mark itself
            // as a structured (recursively-serialized) payload.
            let payload_is_string = matches!(
                &payload_bv,
                BasicValueEnum::StructValue(sv) if {
                    let f = sv.get_type().get_field_types();
                    f.len() == 2
                        && matches!(f[0], BasicTypeEnum::PointerType(_))
                        && matches!(
                            f[1],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        )
                }
            );
            // Option of Result by-value: payload is Result struct {i1,ok,err}.
            if obj_type.contains("Result") && matches!(payload_bv, BasicValueEnum::StructValue(_)) {
                let res_sv = payload_bv.into_struct_value();
                let r_disc = self
                    .build_extract_value(res_sv.into(), 0, "opt_res_disc")?
                    .into_int_value();
                let r_disc_i64 = self
                    .builder
                    .build_int_z_extend(r_disc, self.context.i64_type(), "opt_res_disc_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let r_ok_bv = self.build_extract_value(res_sv.into(), 1, "opt_res_ok")?;
                // Result Ok is product-tuple/record struct — rebuild nested JSON.
                // Inner Result Ok payload may be a struct value OR
                // (`Result<record>`) a pointer to the record — normalize to a
                // record pointer so records stored by pointer serialize
                // recursively like the bytecode VM.
                let (ok_sv_opt, ok_rec_ptr, ok_skip) = match r_ok_bv {
                    BasicValueEnum::StructValue(sv) => {
                        let rec_ty = sv.get_type();
                        let rec_alloca = self
                            .build_alloca(BasicTypeEnum::StructType(rec_ty), "opt_res_rec_tmp")?;
                        self.build_store(rec_alloca, sv)?;
                        let fields = rec_ty.get_field_types();
                        let ok_is_string = fields.len() == 2
                            && matches!(fields[0], BasicTypeEnum::PointerType(_))
                            && matches!(
                                fields[1],
                                BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                            );
                        (Some(sv), Some(rec_alloca), ok_is_string)
                    }
                    BasicValueEnum::PointerValue(pv) => (None, Some(pv), false),
                    _ => (None, None, true),
                };
                if let Some(ok_rec_ptr) = ok_rec_ptr {
                    if !ok_skip {
                        let mut ok_inner = Self::extract_result_ok_type(
                            obj_type
                                .strip_prefix("Option<")
                                .and_then(|s| s.strip_suffix('>'))
                                .unwrap_or(""),
                        );
                        if ok_inner.is_empty() {
                            if let Some(sv) = ok_sv_opt {
                                let pay_sty = sv.get_type();
                                for (n, ty) in &self.type_llvm {
                                    if matches!(
                                        ty,
                                        BasicTypeEnum::StructType(s) if *s == pay_sty
                                    ) && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                    }) {
                                        ok_inner = n.clone();
                                        break;
                                    }
                                }
                            } else if !self.type_llvm.is_empty() {
                                for (n, ty) in &self.type_llvm {
                                    if matches!(ty, BasicTypeEnum::StructType(_))
                                        && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                            matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                        })
                                    {
                                        ok_inner = n.clone();
                                        break;
                                    }
                                }
                            }
                        }
                        let is_named_record = self.type_defs.get(&ok_inner).is_some_and(|td| {
                            matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                        });
                        let ok_is_string = ok_sv_opt.is_some_and(|sv| {
                            let f = sv.get_type().get_field_types();
                            f.len() == 2
                                && matches!(f[0], BasicTypeEnum::PointerType(_))
                                && matches!(f[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64)
                        });
                        let ok_is_product_tuple =
                            ok_inner.starts_with('(') || self.is_product_tuple_alias(&ok_inner);
                        if !is_named_record
                            && !ok_is_string
                            && !ok_is_product_tuple
                            && ok_sv_opt.is_none()
                        {
                            // fall through to i64 path
                        } else {
                            let ok_json = if is_named_record {
                                self.compile_record_to_json_cstr(&ok_inner, ok_rec_ptr)?
                            } else if ok_is_string {
                                let sj = self.emit_heap_string_payload_json(
                                    ok_sv_opt.ok_or_else(|| {
                                        CompileError::Generic(
                                            "to_json Option Result: string payload missing struct"
                                                .into(),
                                        )
                                    })?,
                                )?;
                                self.register_heap_alloc(sj);
                                sj
                            } else {
                                let sv = if let Some(s) = ok_sv_opt {
                                    s
                                } else {
                                    let rec_bty =
                                        self.llvm_type_for(&Type::Name(ok_inner.clone(), vec![]));
                                    let sty = match rec_bty {
                                        Some(BasicTypeEnum::StructType(s)) => s,
                                        _ => {
                                            return Err(CompileError::LlvmError(format!(
                                            "to_json: cannot resolve Option Result tuple type {}",
                                            ok_inner
                                        )))
                                        }
                                    };
                                    self.build_load(
                                        BasicTypeEnum::StructType(sty),
                                        ok_rec_ptr,
                                        "opt_res_tup_ld",
                                    )?
                                    .into_struct_value()
                                };
                                self.emit_product_tuple_to_json(sv)?
                            };
                            let disc_is_some = self
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    disc_i64,
                                    self.context.i64_type().const_int(0, false),
                                    "opt_res_tup_is_some",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let r_is_ok = self
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    r_disc_i64,
                                    self.context.i64_type().const_int(0, false),
                                    "opt_res_tup_is_ok",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let function = self.current_function().ok_or("no function")?;
                            let some_bb = self
                                .context
                                .append_basic_block(function, "toj_opt_res_tup_some");
                            let none_bb = self
                                .context
                                .append_basic_block(function, "toj_opt_res_tup_none");
                            let merge_bb = self
                                .context
                                .append_basic_block(function, "toj_opt_res_tup_merge");
                            let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                            let out_alloca = self.build_alloca(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                "toj_opt_res_tup_out",
                            )?;
                            self.builder
                                .build_conditional_branch(disc_is_some, some_bb, none_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(some_bb);
                            let ok_bb = self
                                .context
                                .append_basic_block(function, "toj_opt_res_tup_ok");
                            let err_bb = self
                                .context
                                .append_basic_block(function, "toj_opt_res_tup_err");
                            let some_merge = self
                                .context
                                .append_basic_block(function, "toj_opt_res_tup_sm");
                            self.builder
                                .build_conditional_branch(r_is_ok, ok_bb, err_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(ok_bb);
                            let inner_buf = self.malloc_or_abort(
                                self.context.i64_type().const_int(1024, false),
                                "opt_res_tup_inner",
                            )?;
                            let ifmt = self
                                .builder
                                .build_global_string_ptr("{\"Ok\":[%s]}", "opt_res_tup_ifmt")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let snprintf_fn = self.get_runtime_fn("snprintf")?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(inner_buf),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(1024, false),
                                    ),
                                    BasicMetadataValueEnum::PointerValue(ifmt.as_pointer_value()),
                                    BasicMetadataValueEnum::PointerValue(ok_json),
                                ],
                                "opt_res_tup_isn",
                            )?;
                            let outer_buf = self.malloc_or_abort(
                                self.context.i64_type().const_int(1024, false),
                                "opt_res_tup_outer",
                            )?;
                            let ofmt = self
                                .builder
                                .build_global_string_ptr("{\"Some\":[%s]}", "opt_res_tup_ofmt")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(outer_buf),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(1024, false),
                                    ),
                                    BasicMetadataValueEnum::PointerValue(ofmt.as_pointer_value()),
                                    BasicMetadataValueEnum::PointerValue(inner_buf),
                                ],
                                "opt_res_tup_osn",
                            )?;
                            self.build_store(out_alloca, outer_buf)?;
                            self.builder
                                .build_unconditional_branch(some_merge)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(err_bb);
                            // Err payload: string Err is heap {ptr,len}; scalar is i64.
                            let r_err_bv =
                                self.build_extract_value(res_sv.into(), 2, "opt_res_tup_errv")?;
                            let r_err_i64 = match r_err_bv {
                                BasicValueEnum::IntValue(iv) => {
                                    if iv.get_type().get_bit_width() < 64 {
                                        self.builder
                                            .build_int_s_extend(
                                                iv,
                                                self.context.i64_type(),
                                                "opt_res_tup_err_i64",
                                            )
                                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                    } else {
                                        iv
                                    }
                                }
                                _ => self.context.i64_type().const_int(0, false),
                            };
                            let inner_err = self.emit_result_err_json(r_err_i64, true)?;
                            let ewrap = self.malloc_or_abort(
                                self.context.i64_type().const_int(1024, false),
                                "opt_res_tup_err_outer",
                            )?;
                            let eofmt = self
                                .builder
                                .build_global_string_ptr("{\"Some\":[%s]}", "opt_res_tup_eofmt")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(ewrap),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(1024, false),
                                    ),
                                    BasicMetadataValueEnum::PointerValue(eofmt.as_pointer_value()),
                                    BasicMetadataValueEnum::PointerValue(inner_err),
                                ],
                                "opt_res_tup_eosn",
                            )?;
                            self.build_store(out_alloca, ewrap)?;
                            self.builder
                                .build_unconditional_branch(some_merge)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(some_merge);
                            self.builder
                                .build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(none_bb);
                            let strcpy_fn = self.get_runtime_fn("strcpy")?;
                            let none_heap = self.malloc_or_abort(
                                self.context.i64_type().const_int(8, false),
                                "opt_res_tup_none",
                            )?;
                            let none_lit = self
                                .builder
                                .build_global_string_ptr("\"None\"", "opt_res_tup_none_lit")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_call(
                                strcpy_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(none_heap),
                                    BasicMetadataValueEnum::PointerValue(
                                        none_lit.as_pointer_value(),
                                    ),
                                ],
                                "opt_res_tup_ncpy",
                            )?;
                            self.build_store(out_alloca, none_heap)?;
                            self.builder
                                .build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(merge_bb);
                            let raw = self
                                .build_load(
                                    BasicTypeEnum::PointerType(i8_ptr_ty),
                                    out_alloca,
                                    "opt_res_tup_result",
                                )?
                                .into_pointer_value();
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        }
                    }
                }
                let r_ok = match r_ok_bv {
                    BasicValueEnum::IntValue(iv) => iv,
                    BasicValueEnum::PointerValue(pv) => self
                        .builder
                        .build_ptr_to_int(pv, self.context.i64_type(), "opt_res_ok_ptr")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                    _ => self.context.i64_type().const_int(0, false),
                };
                let r_ok_i64 = if r_ok.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(r_ok, self.context.i64_type(), "opt_res_ok_i64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    r_ok
                };
                let r_err = self
                    .build_extract_value(res_sv.into(), 2, "opt_res_err")?
                    .into_int_value();
                let r_err_i64 = if r_err.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(r_err, self.context.i64_type(), "opt_res_err_i64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    r_err
                };
                // Option of Result of Map/Set: Ok is a handle, not a plain i64.
                let res_json = if obj_type.contains("Map<") {
                    let mode = if obj_type.contains("Map<string, string>") {
                        1i64
                    } else if obj_type.contains("Map<string, bool>") {
                        2
                    } else if obj_type.contains("Map<string, f64>")
                        || obj_type.contains("Map<string, f32>")
                    {
                        3
                    } else {
                        self.map_nested_product_mode(&obj_type)
                    };
                    let res_fn = self.get_runtime_fn("mimi_result_map_to_json")?;
                    self.build_call(
                        res_fn,
                        &[
                            BasicMetadataValueEnum::IntValue(r_disc_i64),
                            BasicMetadataValueEnum::IntValue(r_ok_i64),
                            BasicMetadataValueEnum::IntValue(r_err_i64),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "opt_res_map_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("result map to_json void")?
                    .into_pointer_value()
                } else if obj_type.contains("Set<") {
                    let mode = if obj_type.contains("Set<string>") {
                        1i64
                    } else if obj_type.contains("Set<bool>") {
                        2
                    } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>") {
                        3
                    } else if let Some(elem) = obj_type
                        .find("Set<")
                        .map(|i| &obj_type[i + 4..])
                        .and_then(|s| {
                            let mut depth = 0i32;
                            for (j, ch) in s.char_indices() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' if depth == 0 => return Some(&s[..j]),
                                    '>' | ')' => depth -= 1,
                                    _ => {}
                                }
                            }
                            None
                        })
                    {
                        if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                            let resolved = if self.is_product_tuple_alias(elem) {
                                self.resolve_alias_type_name(elem)
                            } else {
                                elem.to_string()
                            };
                            let mut arity: i64 = 0;
                            let mut depth = 0i32;
                            let mut any = false;
                            let body = resolved
                                .strip_prefix('(')
                                .and_then(|s| s.strip_suffix(')'))
                                .unwrap_or(resolved.as_str());
                            for ch in body.chars() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        arity += 1;
                                        any = true;
                                    }
                                    c if !c.is_whitespace() => any = true,
                                    _ => {}
                                }
                            }
                            if any {
                                arity += 1;
                            }
                            10 + arity.max(1)
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let res_fn = self.get_runtime_fn("mimi_result_set_to_json")?;
                    self.build_call(
                        res_fn,
                        &[
                            BasicMetadataValueEnum::IntValue(r_disc_i64),
                            BasicMetadataValueEnum::IntValue(r_ok_i64),
                            BasicMetadataValueEnum::IntValue(r_err_i64),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "opt_res_set_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("result set to_json void")?
                    .into_pointer_value()
                } else {
                    // Prefer structured Result JSON so string Err
                    // (heap {ptr,len}) is not printed as a raw i64.
                    self.emit_result_struct_to_json_cstr(res_sv, {
                        obj_type
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                            .unwrap_or("Result")
                    })?
                };
                let disc_is_some = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        disc_i64,
                        self.context.i64_type().const_int(0, false),
                        "opt_res_is_some",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let function = self.current_function().ok_or("no function")?;
                let some_bb = self
                    .context
                    .append_basic_block(function, "toj_opt_res_some");
                let none_bb = self
                    .context
                    .append_basic_block(function, "toj_opt_res_none");
                let merge_bb = self
                    .context
                    .append_basic_block(function, "toj_opt_res_merge");
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let out_alloca =
                    self.build_alloca(BasicTypeEnum::PointerType(i8_ptr_ty), "toj_opt_res_out")?;
                self.builder
                    .build_conditional_branch(disc_is_some, some_bb, none_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(some_bb);
                let buf = self.malloc_or_abort(
                    self.context.i64_type().const_int(512, false),
                    "opt_res_buf",
                )?;
                let fmt = self
                    .builder
                    .build_global_string_ptr("{\"Some\":[%s]}", "opt_res_fmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(
                            self.context.i64_type().const_int(512, false),
                        ),
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(res_json),
                    ],
                    "opt_res_sn",
                )?;
                self.build_store(out_alloca, buf)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(none_bb);
                let none_heap = self.malloc_or_abort(
                    self.context.i64_type().const_int(8, false),
                    "opt_res_none_heap",
                )?;
                let none_lit = self
                    .builder
                    .build_global_string_ptr("\"None\"", "opt_res_none")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                self.build_call(
                    strcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(none_heap),
                        BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                    ],
                    "opt_res_none_cpy",
                )?;
                self.build_store(out_alloca, none_heap)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let raw = self
                    .build_load(
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        out_alloca,
                        "opt_res_result",
                    )?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            // Option of product tuple / named record. A `StructValue` payload is a
            // product tuple, a `String`, a `List`, or a nested `Result`/`Option`;
            // a `PointerValue` payload is exclusively a `**record` (or `**tuple`)
            // — string/list/result payloads are always struct *values*, never
            // pointers. We normalize every payload to a *record pointer* and defer
            // all struct loads to the `Some` path, so the `None` path never
            // dereferences the (NULL) record pointer — matching the bytecode VM,
            // which prints `"None"`.
            let opt_inner = obj_type
                .strip_prefix("Option<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or("")
                .to_string();
            // Option-shaped value whose display name is not a known type (the
            // `None` variant surfaces as a bare name) — still wrap it Some/None.
            let force_option_wrap = arg_is_option_shape
                && !self.type_defs.contains_key(&obj_type)
                && !obj_type.starts_with("Result");
            let (rec_ptr, pay_sv_opt, skip_record_block) = match payload_bv {
                BasicValueEnum::StructValue(sv) => {
                    let rec_ty = sv.get_type();
                    let rec_alloca =
                        self.build_alloca(BasicTypeEnum::StructType(rec_ty), "opt_rec_tmp")?;
                    self.build_store(rec_alloca, sv)?;
                    // Classify struct-value payloads that must NOT take the
                    // record/tuple path (they are handled by earlier blocks).
                    let fields = rec_ty.get_field_types();
                    let pay_is_string = fields.len() == 2
                        && matches!(fields[0], BasicTypeEnum::PointerType(_))
                        && matches!(
                            fields[1],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                        );
                    let pay_is_nested = !fields.is_empty()
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 1
                        );
                    let pay_is_list = fields.len() == 2
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                        )
                        && matches!(fields[1], BasicTypeEnum::PointerType(_));
                    (
                        Some(rec_alloca),
                        Some(sv),
                        pay_is_string || pay_is_nested || pay_is_list,
                    )
                }
                BasicValueEnum::PointerValue(pv) => {
                    // Native codegen stores `Option<record>` as `{i1, ptr}` where
                    // the payload slot holds a *direct* pointer to the record data.
                    // Use it as-is — no deref (the `None` case is a NULL pointer and
                    // must not be read; the `Some` path passes it straight to
                    // `compile_record_to_json_cstr`, which reads via struct-GEP).
                    (Some(pv), None, false)
                }
                _ => (None, None, true),
            };
            if let Some(rec_ptr) = rec_ptr {
                if !skip_record_block {
                    let mut inner_name = opt_inner.clone();
                    // Bare `Option` / missing generic / bare variant name: recover
                    // the named record from the payload LLVM layout, falling back to
                    // the first registered record (only relevant for the forced
                    // None path, where the name is never dereferenced).
                    if inner_name.is_empty() || inner_name == "Option" {
                        let mut recovered = false;
                        if let Some(sv) = pay_sv_opt {
                            let pay_sty = sv.get_type();
                            for (n, ty) in &self.type_llvm {
                                if matches!(ty, BasicTypeEnum::StructType(s) if *s == pay_sty)
                                    && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                    })
                                {
                                    inner_name = n.clone();
                                    recovered = true;
                                    break;
                                }
                            }
                        }
                        if !recovered {
                            for (n, ty) in &self.type_llvm {
                                if matches!(ty, BasicTypeEnum::StructType(_))
                                    && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                    })
                                {
                                    inner_name = n.clone();
                                    break;
                                }
                            }
                        }
                    }
                    let is_named_record = self
                        .type_defs
                        .get(&inner_name)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)));
                    let is_product_tuple =
                        inner_name.starts_with('(') || self.is_product_tuple_alias(&inner_name);
                    if force_option_wrap || is_named_record || is_product_tuple {
                        let disc_is_some = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                disc_i64,
                                self.context.i64_type().const_int(0, false),
                                "opt_tup_is_some",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let function = self.current_function().ok_or("no function")?;
                        let some_bb = self
                            .context
                            .append_basic_block(function, "toj_opt_tup_some");
                        let none_bb = self
                            .context
                            .append_basic_block(function, "toj_opt_tup_none");
                        let merge_bb = self
                            .context
                            .append_basic_block(function, "toj_opt_tup_merge");
                        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let out_alloca = self.build_alloca(
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            "toj_opt_tup_out",
                        )?;
                        self.builder
                            .build_conditional_branch(disc_is_some, some_bb, none_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(some_bb);
                        // Serialize the payload only on the Some path — for None the
                        // record pointer is NULL and must not be dereferenced
                        // (matches the bytecode VM, which prints `"None"`).
                        let pay_json = if is_named_record {
                            self.compile_record_to_json_cstr(&inner_name, rec_ptr)?
                        } else if is_product_tuple {
                            let sv = if let Some(s) = pay_sv_opt {
                                s
                            } else {
                                let rec_bty =
                                    self.llvm_type_for(&Type::Name(inner_name.clone(), vec![]));
                                let sty = match rec_bty {
                                    Some(BasicTypeEnum::StructType(s)) => s,
                                    _ => {
                                        return Err(CompileError::LlvmError(format!(
                                            "to_json: cannot resolve tuple type {}",
                                            inner_name
                                        )))
                                    }
                                };
                                self.build_load(
                                    BasicTypeEnum::StructType(sty),
                                    rec_ptr,
                                    "opt_tup_ld",
                                )?
                                .into_struct_value()
                            };
                            self.emit_product_tuple_to_json(sv)?
                        } else {
                            // Nested container (`Option<Result<…>>`,
                            // `Option<List<…>>`, …): dispatch to_json on the
                            // inner payload with its own type name, then wrap in
                            // `{"Some":[…]}` — exactly like the bytecode VM.
                            // The inner dispatch returns a Mimi-string struct
                            // `{ptr, len}`; extract the raw `*char` (field 0) to
                            // feed `snprintf`'s `%s`, matching the other branches
                            // which already yield a raw pointer.
                            let inner_val = BasicMetadataValueEnum::PointerValue(rec_ptr);
                            match self.emit_typed_to_json_dispatch(&opt_inner, inner_val, None)? {
                                Some(j) => match j {
                                    BasicValueEnum::PointerValue(p) => p,
                                    BasicValueEnum::StructValue(s) => self
                                        .build_extract_value(s.into(), 0, "nested_json_ptr")?
                                        .into_pointer_value(),
                                    other => other.into_pointer_value(),
                                },
                                None => return Ok(None),
                            }
                        };
                        let buf = self.malloc_or_abort(
                            self.context.i64_type().const_int(1024, false),
                            "opt_tup_buf",
                        )?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr("{\"Some\":[%s]}", "opt_tup_fmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let snprintf_fn = self.get_runtime_fn("snprintf")?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(1024, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(pay_json),
                            ],
                            "opt_tup_sn",
                        )?;
                        self.build_store(out_alloca, buf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(none_bb);
                        let none_heap = self.malloc_or_abort(
                            self.context.i64_type().const_int(8, false),
                            "opt_tup_none_heap",
                        )?;
                        let none_lit = self
                            .builder
                            .build_global_string_ptr("\"None\"", "opt_tup_none")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let strcpy_fn = self.get_runtime_fn("strcpy")?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(none_heap),
                                BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                            ],
                            "opt_tup_none_cpy",
                        )?;
                        self.build_store(out_alloca, none_heap)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(merge_bb);
                        let raw = self
                            .build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "opt_tup_result",
                            )?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                }
            }
            // Option of named record: pointer payload (Some stores stack
            // alloca of record as ptr) or i64 ptrtoint.
            if let Some(inner_name) = obj_type
                .strip_prefix("Option<")
                .and_then(|s| s.strip_suffix('>'))
            {
                if self
                    .type_defs
                    .get(inner_name)
                    .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
                {
                    if let BasicValueEnum::PointerValue(rec_ptr) = payload_bv {
                        let disc_is_some = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                disc_i64,
                                self.context.i64_type().const_int(0, false),
                                "opt_rec_ptr_is_some",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let function = self.current_function().ok_or("no function")?;
                        let some_bb = self
                            .context
                            .append_basic_block(function, "toj_opt_rec_ptr_some");
                        let none_bb = self
                            .context
                            .append_basic_block(function, "toj_opt_rec_ptr_none");
                        let merge_bb = self
                            .context
                            .append_basic_block(function, "toj_opt_rec_ptr_merge");
                        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let out_alloca = self.build_alloca(
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            "toj_opt_rec_ptr_out",
                        )?;
                        self.builder
                            .build_conditional_branch(disc_is_some, some_bb, none_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(some_bb);
                        let rec_json = self.compile_record_to_json_cstr(inner_name, rec_ptr)?;
                        let buf = self.malloc_or_abort(
                            self.context.i64_type().const_int(1024, false),
                            "opt_rec_ptr_buf",
                        )?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr("{\"Some\":[%s]}", "opt_rec_ptr_fmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let snprintf_fn = self.get_runtime_fn("snprintf")?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(1024, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(rec_json),
                            ],
                            "opt_rec_ptr_sn",
                        )?;
                        self.build_store(out_alloca, buf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(none_bb);
                        let none_heap = self.malloc_or_abort(
                            self.context.i64_type().const_int(8, false),
                            "opt_rec_ptr_none",
                        )?;
                        let none_lit = self
                            .builder
                            .build_global_string_ptr("\"None\"", "opt_rec_ptr_none_lit")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let strcpy_fn = self.get_runtime_fn("strcpy")?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(none_heap),
                                BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                            ],
                            "opt_rec_ptr_none_cpy",
                        )?;
                        self.build_store(out_alloca, none_heap)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(merge_bb);
                        let raw = self
                            .build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "opt_rec_ptr_result",
                            )?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                    if let BasicValueEnum::IntValue(pay_iv) = payload_bv {
                        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let pay_i64 = if pay_iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(
                                    pay_iv,
                                    self.context.i64_type(),
                                    "opt_rec_pay_i64",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            pay_iv
                        };
                        let disc_is_some = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                disc_i64,
                                self.context.i64_type().const_int(0, false),
                                "opt_rec_is_some",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let function = self.current_function().ok_or("no function")?;
                        let some_bb = self
                            .context
                            .append_basic_block(function, "toj_opt_rec_some");
                        let none_bb = self
                            .context
                            .append_basic_block(function, "toj_opt_rec_none");
                        let merge_bb = self
                            .context
                            .append_basic_block(function, "toj_opt_rec_merge");
                        let out_alloca = self.build_alloca(
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            "toj_opt_rec_out",
                        )?;
                        self.builder
                            .build_conditional_branch(disc_is_some, some_bb, none_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(some_bb);
                        let rec_ptr = self
                            .builder
                            .build_int_to_ptr(pay_i64, i8_ptr_ty, "opt_rec_ptr")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let rec_json = self.compile_record_to_json_cstr(inner_name, rec_ptr)?;
                        let buf = self.malloc_or_abort(
                            self.context.i64_type().const_int(1024, false),
                            "opt_rec_buf",
                        )?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr("{\"Some\":[%s]}", "opt_rec_fmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let snprintf_fn = self.get_runtime_fn("snprintf")?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(1024, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(rec_json),
                            ],
                            "opt_rec_sn",
                        )?;
                        self.build_store(out_alloca, buf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(none_bb);
                        let none_heap = self.malloc_or_abort(
                            self.context.i64_type().const_int(8, false),
                            "opt_rec_none_heap",
                        )?;
                        let none_lit = self
                            .builder
                            .build_global_string_ptr("\"None\"", "opt_rec_none")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let strcpy_fn = self.get_runtime_fn("strcpy")?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(none_heap),
                                BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                            ],
                            "opt_rec_none_cpy",
                        )?;
                        self.build_store(out_alloca, none_heap)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(merge_bb);
                        let raw = self
                            .build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "opt_rec_result",
                            )?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    }
                }
            }
            // Nested Option / List by-value as StructValue payload.
            if let BasicValueEnum::StructValue(pay_sv) = payload_bv {
                let pay_fields = pay_sv.get_type().get_field_types();
                // Option of List by-value: {i64,ptr} list struct.
                let pay_is_list = pay_fields.len() == 2
                    && matches!(
                        pay_fields[0],
                        BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                    )
                    && matches!(pay_fields[1], BasicTypeEnum::PointerType(_));
                if pay_is_list && (obj_type.contains("List") || obj_type.contains("list")) {
                    let list_ty = self.list_struct_type();
                    let list_alloca =
                        self.build_alloca(BasicTypeEnum::StructType(list_ty), "opt_list_bv")?;
                    self.build_store(list_alloca, pay_sv)?;
                    // Reuse Option of List path: build {"Some":[list_json]} / None.
                    let disc_is_some = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            disc_i64,
                            self.context.i64_type().const_int(0, false),
                            "opt_list_bv_some",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let function = self.current_function().ok_or("no function")?;
                    let some_bb = self
                        .context
                        .append_basic_block(function, "toj_opt_list_bv_some");
                    let none_bb = self
                        .context
                        .append_basic_block(function, "toj_opt_list_bv_none");
                    let merge_bb = self
                        .context
                        .append_basic_block(function, "toj_opt_list_bv_merge");
                    let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let out_alloca = self.build_alloca(
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        "toj_opt_list_bv_out",
                    )?;
                    self.builder
                        .build_conditional_branch(disc_is_some, some_bb, none_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(some_bb);
                    // Dispatch list to_json helper by element type.
                    let list_json = if obj_type.contains("Map<") {
                        // Option of List of Map of product.
                        // Accept both `Map<string, (…)>` and `Map<string,(…)>`.
                        let map_val = obj_type
                            .find("Map<string,")
                            .map(|i| &obj_type[i + "Map<string,".len()..])
                            .map(|s| s.trim_start())
                            .and_then(|s| {
                                // Take until matching '>' for Map value type.
                                let mut depth = 0i32;
                                for (j, ch) in s.char_indices() {
                                    match ch {
                                        '<' | '(' => depth += 1,
                                        '>' if depth == 0 => {
                                            return Some(s[..j].trim());
                                        }
                                        '>' | ')' => depth -= 1,
                                        _ => {}
                                    }
                                }
                                None
                            });
                        if let Some(val_ty) = map_val {
                            if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                let elem = if self.is_product_tuple_alias(val_ty) {
                                    self.resolve_alias_type_name(val_ty)
                                } else {
                                    val_ty.to_string()
                                };
                                self.emit_list_map_product_to_json(list_alloca, &elem)?
                            } else {
                                let mode = if obj_type.contains("Map<string, string>") {
                                    1i64
                                } else if obj_type.contains("Map<string, bool>") {
                                    2
                                } else if obj_type.contains("Map<string, f64>")
                                    || obj_type.contains("Map<string, f32>")
                                {
                                    3
                                } else {
                                    0
                                };
                                let fn_ty = i8_ptr_ty.fn_type(
                                    &[
                                        BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                                        BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                                    ],
                                    false,
                                );
                                let callee = self
                                    .module
                                    .get_function("mimi_list_map_to_json")
                                    .unwrap_or_else(|| {
                                        self.module.add_function(
                                            "mimi_list_map_to_json",
                                            fn_ty,
                                            Some(inkwell::module::Linkage::External),
                                        )
                                    });
                                self.build_call(
                                    callee,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(list_alloca),
                                        BasicMetadataValueEnum::IntValue(
                                            self.context.i64_type().const_int(mode as u64, false),
                                        ),
                                    ],
                                    "opt_list_map_json",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("list map to_json void")?
                                .into_pointer_value()
                            }
                        } else {
                            let map_fn = if obj_type.contains("Map<string, string>") {
                                "mimi_list_map_to_json_string"
                            } else {
                                "mimi_list_map_to_string"
                            };
                            let map_callee =
                                self.module.get_function(map_fn).unwrap_or_else(|| {
                                    let fn_ty = i8_ptr_ty.fn_type(
                                        &[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)],
                                        false,
                                    );
                                    self.module.add_function(
                                        map_fn,
                                        fn_ty,
                                        Some(inkwell::module::Linkage::External),
                                    )
                                });
                            self.build_call(
                                map_callee,
                                &[BasicMetadataValueEnum::PointerValue(list_alloca)],
                                "opt_list_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list map to_json void")?
                            .into_pointer_value()
                        }
                    } else {
                        // Reuse the unified `to_json` dispatch for the inner
                        // `List<elem>` — this is exactly the path a bare
                        // `List<elem>` takes, so it handles scalar, product-tuple,
                        // and nested-list elements uniformly. The previous hard-coded
                        // `mimi_list_i64_to_json` fallback mis-serialized non-scalar
                        // elements (e.g. `List<(i32, i32)>` product tuples) as raw
                        // i64, producing garbage JSON.
                        let inner_list = obj_type
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                            .unwrap_or_else(|| obj_type.as_str());
                        let elem_ty = inner_list
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                            .unwrap_or_else(|| inner_list)
                            .to_string();
                        let inner_val = BasicMetadataValueEnum::PointerValue(list_alloca);
                        let list_json = match self
                            .emit_typed_to_json_dispatch(&format!("List<{}>", elem_ty), inner_val, None)?
                        {
                            Some(j) => match j {
                                BasicValueEnum::PointerValue(p) => p,
                                BasicValueEnum::StructValue(s) => self
                                    .build_extract_value(s.into(), 0, "opt_list_inner_json_ptr")?
                                    .into_pointer_value(),
                                other => other.into_pointer_value(),
                            },
                            None => return Ok(None),
                        };
                        list_json
                    };
                    let buf = self.malloc_or_abort(
                        self.context.i64_type().const_int(1024, false),
                        "opt_list_bv_buf",
                    )?;
                    let fmt = self
                        .builder
                        .build_global_string_ptr("{\"Some\":[%s]}", "opt_list_bv_fmt")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let snprintf_fn = self.get_runtime_fn("snprintf")?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(1024, false),
                            ),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(list_json),
                        ],
                        "opt_list_bv_sn",
                    )?;
                    self.build_store(out_alloca, buf)?;
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(none_bb);
                    let none_heap = self.malloc_or_abort(
                        self.context.i64_type().const_int(8, false),
                        "opt_list_bv_none",
                    )?;
                    let none_lit = self
                        .builder
                        .build_global_string_ptr("\"None\"", "opt_list_bv_none_lit")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let strcpy_fn = self.get_runtime_fn("strcpy")?;
                    self.build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(none_heap),
                            BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                        ],
                        "opt_list_bv_ncpy",
                    )?;
                    self.build_store(out_alloca, none_heap)?;
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(merge_bb);
                    let raw = self
                        .build_load(
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            out_alloca,
                            "opt_list_bv_result",
                        )?
                        .into_pointer_value();
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
                if !pay_fields.is_empty()
                    && matches!(
                        pay_fields[0],
                        BasicTypeEnum::IntType(it) if it.get_bit_width() == 1
                    )
                    && obj_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .is_some_and(|inner| inner.starts_with("Option"))
                {
                    // Heap-pack nested Option and reuse nested path via i64.
                    let sty = pay_sv.get_type();
                    let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(sty));
                    let heap = self.malloc_or_abort(
                        self.context.i64_type().const_int(size, false),
                        "opt_nest_bv_heap",
                    )?;
                    let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                    let typed = self
                        .build_bit_cast(
                            heap.into(),
                            BasicTypeEnum::PointerType(i8_ptr),
                            "opt_nest_bv_ptr",
                        )?
                        .into_pointer_value();
                    self.build_store(typed, pay_sv)?;
                    let payload_i64 = self
                        .builder
                        .build_ptr_to_int(typed, self.context.i64_type(), "opt_nest_bv_i64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    // Fall through into nested Option rebuild using payload_i64.
                    let disc_is_some = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            disc_i64,
                            self.context.i64_type().const_int(0, false),
                            "opt_nest_bv_some",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let function = self.current_function().ok_or("no function")?;
                    let some_bb = self
                        .context
                        .append_basic_block(function, "toj_opt_nest_bv_some");
                    let none_bb = self
                        .context
                        .append_basic_block(function, "toj_opt_nest_bv_none");
                    let merge_bb = self
                        .context
                        .append_basic_block(function, "toj_opt_nest_bv_merge");
                    let out_alloca = self
                        .build_alloca(BasicTypeEnum::PointerType(i8_ptr), "toj_opt_nest_bv_out")?;
                    self.builder
                        .build_conditional_branch(disc_is_some, some_bb, none_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(some_bb);
                    let nested_ptr = self
                        .builder
                        .build_int_to_ptr(payload_i64, i8_ptr, "opt_nest_bv_ld_ptr")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let opt_sty = self.context.struct_type(
                        &[
                            self.context.bool_type().into(),
                            self.context.i64_type().into(),
                        ],
                        false,
                    );
                    let nested_sv = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(opt_sty),
                            nested_ptr,
                            "opt_nest_bv_ld",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_struct_value();
                    let n_disc = self
                        .build_extract_value(nested_sv.into(), 0, "n_disc_bv")?
                        .into_int_value();
                    let n_disc_i64 = self
                        .builder
                        .build_int_z_extend(n_disc, self.context.i64_type(), "n_disc_bv_i64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let n_pay = self
                        .build_extract_value(nested_sv.into(), 1, "n_pay_bv")?
                        .into_int_value();
                    let n_pay_i64 = if n_pay.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(n_pay, self.context.i64_type(), "n_pay_bv_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        n_pay
                    };
                    let opt_fn = self.get_runtime_fn("mimi_option_i64_to_json")?;
                    let nested_json = self
                        .build_call(
                            opt_fn,
                            &[
                                BasicMetadataValueEnum::IntValue(n_disc_i64),
                                BasicMetadataValueEnum::IntValue(n_pay_i64),
                            ],
                            "opt_nest_bv_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("option to_json void")?
                        .into_pointer_value();
                    let buf = self.malloc_or_abort(
                        self.context.i64_type().const_int(512, false),
                        "opt_nest_bv_buf",
                    )?;
                    let fmt = self
                        .builder
                        .build_global_string_ptr("{\"Some\":[%s]}", "opt_nest_bv_fmt")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let snprintf_fn = self.get_runtime_fn("snprintf")?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(512, false),
                            ),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(nested_json),
                        ],
                        "opt_nest_bv_sn",
                    )?;
                    self.build_store(out_alloca, buf)?;
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(none_bb);
                    let none_heap = self.malloc_or_abort(
                        self.context.i64_type().const_int(8, false),
                        "opt_nest_bv_none",
                    )?;
                    let none_lit = self
                        .builder
                        .build_global_string_ptr("\"None\"", "opt_nest_bv_none_lit")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let strcpy_fn = self.get_runtime_fn("strcpy")?;
                    self.build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(none_heap),
                            BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                        ],
                        "opt_nest_bv_ncpy",
                    )?;
                    self.build_store(out_alloca, none_heap)?;
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(merge_bb);
                    let raw = self
                        .build_load(
                            BasicTypeEnum::PointerType(i8_ptr),
                            out_alloca,
                            "opt_nest_bv_result",
                        )?
                        .into_pointer_value();
                    self.register_heap_alloc(raw);
                    return Ok(Some(self.wrap_c_string(raw)?));
                }
            }
            let payload_i64 = match payload_bv {
                BasicValueEnum::IntValue(iv) => {
                    if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(iv, self.context.i64_type(), "opt_pay_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    }
                }
                BasicValueEnum::PointerValue(pv) => self
                    .builder
                    .build_ptr_to_int(pv, self.context.i64_type(), "opt_pay_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                BasicValueEnum::StructValue(sv) => {
                    // D-3: heap-string payload {ptr,i64} — serialize
                    // to a JSON string literal instead of the generic
                    // E0700 rejection.
                    let j = self.emit_heap_string_payload_json(sv)?;
                    self.register_heap_alloc(j);
                    self.builder
                        .build_ptr_to_int(j, self.context.i64_type(), "opt_pay_str_json")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                }
                other => {
                    return Err(CompileError::Generic(format!(
                        "to_json Option: unexpected payload {:?}",
                        other.get_type()
                    )));
                }
            };
            if obj_type
                .strip_prefix("Option<")
                .and_then(|s| s.strip_suffix('>'))
                .is_some_and(|inner| inner.starts_with("Option"))
            {
                // Nested Option: payload is ptrtoint of heap Option {i1,i64}.
                // mimi_option_i64_to_json only handles int payloads — rebuild.
                let disc_is_some = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        disc_i64,
                        self.context.i64_type().const_int(0, false),
                        "opt_nest_some",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let function = self.current_function().ok_or("no function")?;
                let some_bb = self
                    .context
                    .append_basic_block(function, "toj_opt_nest_some");
                let none_bb = self
                    .context
                    .append_basic_block(function, "toj_opt_nest_none");
                let merge_bb = self
                    .context
                    .append_basic_block(function, "toj_opt_nest_merge");
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let out_alloca =
                    self.build_alloca(BasicTypeEnum::PointerType(i8_ptr_ty), "toj_opt_nest_out")?;
                self.builder
                    .build_conditional_branch(disc_is_some, some_bb, none_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(some_bb);
                let nested_ptr = self
                    .builder
                    .build_int_to_ptr(payload_i64, i8_ptr_ty, "opt_nest_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let opt_sty = self.context.struct_type(
                    &[
                        self.context.bool_type().into(),
                        self.context.i64_type().into(),
                    ],
                    false,
                );
                let nested_sv = self
                    .builder
                    .build_load(
                        BasicTypeEnum::StructType(opt_sty),
                        nested_ptr,
                        "opt_nest_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                let n_disc = self
                    .build_extract_value(nested_sv.into(), 0, "n_disc")?
                    .into_int_value();
                let n_disc_i64 = self
                    .builder
                    .build_int_z_extend(n_disc, self.context.i64_type(), "n_disc_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let n_pay = self
                    .build_extract_value(nested_sv.into(), 1, "n_pay")?
                    .into_int_value();
                let n_pay_i64 = if n_pay.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(n_pay, self.context.i64_type(), "n_pay_i64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    n_pay
                };
                let func = self.get_runtime_fn("mimi_option_i64_to_json")?;
                let inner_json = self
                    .build_call(
                        func,
                        &[
                            BasicMetadataValueEnum::IntValue(n_disc_i64),
                            BasicMetadataValueEnum::IntValue(n_pay_i64),
                        ],
                        "opt_nest_inner_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("option to_json void")?
                    .into_pointer_value();
                let buf = self.malloc_or_abort(
                    self.context.i64_type().const_int(512, false),
                    "opt_nest_buf",
                )?;
                let fmt = self
                    .builder
                    .build_global_string_ptr("{\"Some\":[%s]}", "opt_nest_fmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(
                            self.context.i64_type().const_int(512, false),
                        ),
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(inner_json),
                    ],
                    "opt_nest_sn",
                )?;
                self.build_store(out_alloca, buf)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(none_bb);
                // Heap-copy "None" so wrap_c_string free is always valid.
                let none_heap = self.malloc_or_abort(
                    self.context.i64_type().const_int(8, false),
                    "opt_nest_none_heap",
                )?;
                let none_lit = self
                    .builder
                    .build_global_string_ptr("\"None\"", "opt_nest_none")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                self.build_call(
                    strcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(none_heap),
                        BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                    ],
                    "opt_nest_none_cpy",
                )?;
                self.build_store(out_alloca, none_heap)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let raw = self
                    .build_load(
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        out_alloca,
                        "opt_nest_result",
                    )?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            if obj_type.contains("List<") {
                // Option of List: payload is pointer to list struct
                // (or ptrtoint of it). Element type may be Map/Set/scalar.
                let disc_is_some = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        disc_i64,
                        self.context.i64_type().const_int(0, false),
                        "opt_list_some",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let function = self.current_function().ok_or("no function")?;
                let some_bb = self
                    .context
                    .append_basic_block(function, "toj_opt_list_some");
                let none_bb = self
                    .context
                    .append_basic_block(function, "toj_opt_list_none");
                let merge_bb = self
                    .context
                    .append_basic_block(function, "toj_opt_list_merge");
                let out_alloca = self.build_alloca(
                    BasicTypeEnum::PointerType(
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                    ),
                    "toj_opt_list_out",
                )?;
                self.builder
                    .build_conditional_branch(disc_is_some, some_bb, none_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(some_bb);
                let list_ptr = self
                    .builder
                    .build_int_to_ptr(
                        payload_i64,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "opt_list_as_ptr",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                // Product-tuple list elements need codegen JSON helpers.
                let list_inner = obj_type
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                    .and_then(|s| s.strip_prefix("List<"))
                    .and_then(|s| s.strip_suffix('>'))
                    .unwrap_or("");
                let list_json = if list_inner.starts_with("List<") {
                    let mid_elem =
                        Self::strip_first_type_arg(&format!("List<{}>", list_inner), "List")
                            .and_then(|mid| Self::strip_first_type_arg(&mid, "List"))
                            .unwrap_or_else(|| list_inner.to_string());
                    if mid_elem.starts_with('(') || self.is_product_tuple_alias(&mid_elem) {
                        let elem = if self.is_product_tuple_alias(&mid_elem) {
                            self.resolve_alias_type_name(&mid_elem)
                        } else {
                            mid_elem
                        };
                        self.emit_list_list_product_tuple_to_json(list_ptr, &elem)?
                    } else {
                        // fall through to scalar helpers below
                        let list_fn = self.get_runtime_fn("mimi_list_i64_to_json")?;
                        self.build_call(
                            list_fn,
                            &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                            "opt_list_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list to_json void")?
                        .into_pointer_value()
                    }
                } else if list_inner.starts_with('(') || self.is_product_tuple_alias(list_inner) {
                    let elem = if self.is_product_tuple_alias(list_inner) {
                        self.resolve_alias_type_name(list_inner)
                    } else {
                        list_inner.to_string()
                    };
                    self.emit_list_product_tuple_to_json(list_ptr, &elem)?
                } else if list_inner.starts_with("Map") {
                    if let Some(val_ty) = list_inner
                        .strip_prefix("Map<string, ")
                        .and_then(|s| s.strip_suffix('>'))
                        .or_else(|| {
                            list_inner
                                .strip_prefix("Map<string,")
                                .and_then(|s| s.strip_suffix('>'))
                                .map(|s| s.trim())
                        })
                    {
                        if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                            let elem = if self.is_product_tuple_alias(val_ty) {
                                self.resolve_alias_type_name(val_ty)
                            } else {
                                val_ty.to_string()
                            };
                            self.emit_list_map_product_to_json(list_ptr, &elem)?
                        } else {
                            // Value type is string only when Map<string, string>.
                            let list_fn_name = if list_inner.contains("Map<string, string>") {
                                "mimi_list_map_to_json_string"
                            } else {
                                "mimi_list_map_to_string"
                            };
                            let list_fn = self.get_runtime_fn(list_fn_name)?;
                            self.build_call(
                                list_fn,
                                &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                                "opt_list_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list map to_json void")?
                            .into_pointer_value()
                        }
                    } else {
                        let list_fn = self.get_runtime_fn("mimi_list_map_to_string")?;
                        self.build_call(
                            list_fn,
                            &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                            "opt_list_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list map to_json void")?
                        .into_pointer_value()
                    }
                } else if list_inner.starts_with("Set") {
                    if let Some(elem) = list_inner
                        .strip_prefix("Set<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                            let resolved = if self.is_product_tuple_alias(elem) {
                                self.resolve_alias_type_name(elem)
                            } else {
                                elem.to_string()
                            };
                            let mut arity: i64 = 0;
                            let mut depth = 0i32;
                            let mut any = false;
                            let body = resolved
                                .strip_prefix('(')
                                .and_then(|s| s.strip_suffix(')'))
                                .unwrap_or(resolved.as_str());
                            for ch in body.chars() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        arity += 1;
                                        any = true;
                                    }
                                    c if !c.is_whitespace() => any = true,
                                    _ => {}
                                }
                            }
                            if any {
                                arity += 1;
                            }
                            let func = self.get_runtime_fn("mimi_list_set_product_to_json")?;
                            self.build_call(
                                func,
                                &[
                                    BasicMetadataValueEnum::PointerValue(list_ptr),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context
                                            .i64_type()
                                            .const_int(arity.max(1) as u64, false),
                                    ),
                                ],
                                "opt_list_set_product_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list set product to_json void")?
                            .into_pointer_value()
                        } else {
                            let list_fn = self.get_runtime_fn("mimi_list_set_to_json")?;
                            self.build_call(
                                list_fn,
                                &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                                "opt_list_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list set to_json void")?
                            .into_pointer_value()
                        }
                    } else {
                        let list_fn = self.get_runtime_fn("mimi_list_set_to_json")?;
                        self.build_call(
                            list_fn,
                            &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                            "opt_list_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list set to_json void")?
                        .into_pointer_value()
                    }
                } else {
                    let list_fn_name =
                        if obj_type.contains("List<Map") || obj_type.contains("List<Map<") {
                            if obj_type.contains("Map<string, string>") {
                                "mimi_list_map_to_json_string"
                            } else {
                                "mimi_list_map_to_string"
                            }
                        } else if obj_type.contains("List<Set") {
                            "mimi_list_set_to_json"
                        } else if obj_type.contains("List<string>") {
                            "mimi_list_str_to_json"
                        } else if obj_type.contains("List<f64>") || obj_type.contains("List<f32>") {
                            "mimi_list_f64_to_json"
                        } else if obj_type.contains("List<bool>") {
                            "mimi_list_bool_to_json"
                        } else {
                            "mimi_list_i64_to_json"
                        };
                    let list_fn_ty =
                        i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
                    let list_fn = self.module.get_function(list_fn_name).unwrap_or_else(|| {
                        self.module.add_function(
                            list_fn_name,
                            list_fn_ty,
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                    self.build_call(
                        list_fn,
                        &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                        "opt_list_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("list to_json void")?
                    .into_pointer_value()
                };
                let buf = self.malloc_or_abort(
                    self.context.i64_type().const_int(4096, false),
                    "opt_list_buf",
                )?;
                let fmt = self
                    .builder
                    .build_global_string_ptr("{\"Some\":[%s]}", "opt_list_fmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(
                            self.context.i64_type().const_int(4096, false),
                        ),
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(list_json),
                    ],
                    "opt_list_sn",
                )?;
                self.build_store(out_alloca, buf)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(none_bb);
                let none_heap = self.malloc_or_abort(
                    self.context.i64_type().const_int(8, false),
                    "opt_list_none_heap",
                )?;
                let none_lit = self
                    .builder
                    .build_global_string_ptr("\"None\"", "opt_list_none")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                self.build_call(
                    strcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(none_heap),
                        BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                    ],
                    "opt_list_none_cpy",
                )?;
                self.build_store(out_alloca, none_heap)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let raw = self
                    .build_load(
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        out_alloca,
                        "opt_list_result",
                    )?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            let opt_inner = obj_type
                .strip_prefix("Option<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or(obj_type.as_str());
            if opt_inner.starts_with("Map<") {
                let mode = if obj_type.contains("Map<string, string>") {
                    1i64
                } else if obj_type.contains("Map<string, bool>") {
                    2
                } else if obj_type.contains("Map<string, f64>")
                    || obj_type.contains("Map<string, f32>")
                {
                    3
                } else {
                    self.map_nested_product_mode(&obj_type)
                };
                let func = self.get_runtime_fn("mimi_option_map_to_json")?;
                let raw = self
                    .build_call(
                        func,
                        &[
                            BasicMetadataValueEnum::IntValue(disc_i64),
                            BasicMetadataValueEnum::IntValue(payload_i64),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "to_json_opt_map",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_option_map_to_json void")?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            if opt_inner.starts_with("Set<") {
                let mode = if obj_type.contains("Set<string>") {
                    1i64
                } else if obj_type.contains("Set<bool>") {
                    2
                } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>") {
                    3
                } else if let Some(elem) = obj_type
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                    .and_then(|s| s.strip_prefix("Set<"))
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                        let resolved = if self.is_product_tuple_alias(elem) {
                            self.resolve_alias_type_name(elem)
                        } else {
                            elem.to_string()
                        };
                        let mut arity: i64 = 0;
                        let mut depth = 0i32;
                        let mut any = false;
                        let body = resolved
                            .strip_prefix('(')
                            .and_then(|s| s.strip_suffix(')'))
                            .unwrap_or(resolved.as_str());
                        for ch in body.chars() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    arity += 1;
                                    any = true;
                                }
                                c if !c.is_whitespace() => any = true,
                                _ => {}
                            }
                        }
                        if any {
                            arity += 1;
                        }
                        10 + arity.max(1)
                    } else if elem.starts_with("Map<string, ") {
                        if let Some(val_ty) = elem
                            .strip_prefix("Map<string, ")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                let resolved = if self.is_product_tuple_alias(val_ty) {
                                    self.resolve_alias_type_name(val_ty)
                                } else {
                                    val_ty.to_string()
                                };
                                let mut arity: i64 = 0;
                                let mut depth = 0i32;
                                let mut any = false;
                                let body = resolved
                                    .strip_prefix('(')
                                    .and_then(|s| s.strip_suffix(')'))
                                    .unwrap_or(resolved.as_str());
                                for ch in body.chars() {
                                    match ch {
                                        '<' | '(' => depth += 1,
                                        '>' | ')' => depth -= 1,
                                        ',' if depth == 0 => {
                                            arity += 1;
                                            any = true;
                                        }
                                        c if !c.is_whitespace() => any = true,
                                        _ => {}
                                    }
                                }
                                if any {
                                    arity += 1;
                                }
                                70 + arity.max(1)
                            } else {
                                0
                            }
                        } else {
                            0
                        }
                    } else {
                        0
                    }
                } else {
                    0
                };
                let func = self.get_runtime_fn("mimi_option_set_to_json")?;
                let raw = self
                    .build_call(
                        func,
                        &[
                            BasicMetadataValueEnum::IntValue(disc_i64),
                            BasicMetadataValueEnum::IntValue(payload_i64),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "to_json_opt_set",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_option_set_to_json void")?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            // D-3: Option<string> — Some payload is a heap string
            // {ptr,i64}; payload_i64 is the ptrtoint of the escaped
            // JSON string literal. Emit structured JSON instead of
            // printing the pointer as a number.
            if payload_is_string {
                let raw = self.emit_option_string_to_json_cstr(disc_i64, payload_i64)?;
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            let func = self.get_runtime_fn("mimi_option_i64_to_json")?;
            let raw = self
                .build_call(
                    func,
                    &[
                        BasicMetadataValueEnum::IntValue(disc_i64),
                        BasicMetadataValueEnum::IntValue(payload_i64),
                    ],
                    "to_json_opt",
                )?
                .try_as_basic_value_opt()
                .ok_or("mimi_option_i64_to_json void")?
                .into_pointer_value();
            self.register_heap_alloc(raw);
            return Ok(Some(self.wrap_c_string(raw)?));
        }
        // Result / Result<T,E> integer payloads: {i1, ok, err}
        if obj_type == "Result" || obj_type.starts_with("Result<") {
            let sv = match &arg0 {
                BasicMetadataValueEnum::StructValue(s) => *s,
                BasicMetadataValueEnum::PointerValue(pv) => {
                    let loaded = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(self.context.struct_type(
                                &[
                                    self.context.bool_type().into(),
                                    self.context.i64_type().into(),
                                    self.context.i64_type().into(),
                                ],
                                false,
                            )),
                            *pv,
                            "res_load",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_struct_value();
                    loaded
                }
                other => {
                    return Err(CompileError::Generic(format!(
                        "to_json: unexpected Result argument kind {:?}",
                        other
                    )))
                }
            };
            let disc = self
                .build_extract_value(sv.into(), 0, "res_disc")?
                .into_int_value();
            let disc_i64 = self
                .builder
                .build_int_z_extend(disc, self.context.i64_type(), "res_disc_i64")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let ok_bv = self.build_extract_value(sv.into(), 1, "res_ok")?;
            // Result of Option: Ok is nested Option struct {i1, payload}.
            // Require Option to be the Ok type root (not List<Option<…>> etc.).
            let result_ok_is_option = obj_type
                .strip_prefix("Result<")
                .map(|s| {
                    let mut depth = 0i32;
                    for (i, ch) in s.char_indices() {
                        match ch {
                            '<' | '(' => depth += 1,
                            '>' | ')' => depth -= 1,
                            ',' if depth == 0 => {
                                return s[..i].trim().starts_with("Option");
                            }
                            _ => {}
                        }
                    }
                    false
                })
                .unwrap_or(false);
            if result_ok_is_option && matches!(ok_bv, BasicValueEnum::StructValue(_)) {
                let opt_sv = ok_bv.into_struct_value();
                let o_disc = self
                    .build_extract_value(opt_sv.into(), 0, "res_opt_disc")?
                    .into_int_value();
                let o_disc_i64 = self
                    .builder
                    .build_int_z_extend(o_disc, self.context.i64_type(), "res_opt_disc_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let o_pay_bv = self.build_extract_value(opt_sv.into(), 1, "res_opt_pay")?;
                // Option of product-tuple/record inside Result: rebuild
                // {"Some":[…]} from payload rather than i64 helper. The inner
                // Option payload may be a struct value OR (`Option<record>`) a
                // pointer to the record — normalize to a record pointer.
                let (pay_rec_ptr, pay_sv_opt, pay_skip) = match o_pay_bv {
                    BasicValueEnum::StructValue(sv) => {
                        let rec_ty = sv.get_type();
                        let rec_alloca = self
                            .build_alloca(BasicTypeEnum::StructType(rec_ty), "res_opt_rec_tmp")?;
                        self.build_store(rec_alloca, sv)?;
                        let fields = rec_ty.get_field_types();
                        let pay_is_string = fields.len() == 2
                            && matches!(fields[0], BasicTypeEnum::PointerType(_))
                            && matches!(
                                fields[1],
                                BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                            );
                        (Some(rec_alloca), Some(sv), pay_is_string)
                    }
                    BasicValueEnum::PointerValue(pv) => (Some(pv), None, false),
                    _ => (None, None, true),
                };
                if let Some(pay_rec_ptr) = pay_rec_ptr {
                    if !pay_skip {
                        let mut pay_inner = Self::extract_result_ok_type(&obj_type)
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default();
                        if pay_inner.is_empty() {
                            if let Some(sv) = pay_sv_opt {
                                let pay_sty = sv.get_type();
                                for (n, ty) in &self.type_llvm {
                                    if matches!(
                                        ty,
                                        BasicTypeEnum::StructType(s) if *s == pay_sty
                                    ) && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                    }) {
                                        pay_inner = n.clone();
                                        break;
                                    }
                                }
                            } else if !self.type_llvm.is_empty() {
                                for (n, ty) in &self.type_llvm {
                                    if matches!(ty, BasicTypeEnum::StructType(_))
                                        && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                            matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                        })
                                    {
                                        pay_inner = n.clone();
                                        break;
                                    }
                                }
                            }
                        }
                        let is_named_record = self.type_defs.get(&pay_inner).is_some_and(|td| {
                            matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                        });
                        let pay_is_string = pay_sv_opt.is_some_and(|sv| {
                            let f = sv.get_type().get_field_types();
                            f.len() == 2
                                && matches!(f[0], BasicTypeEnum::PointerType(_))
                                && matches!(f[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64)
                        });
                        let pay_is_product_tuple =
                            pay_inner.starts_with('(') || self.is_product_tuple_alias(&pay_inner);
                        let pay_is_container = pay_inner.starts_with("Option<")
                            || pay_inner.starts_with("List<")
                            || pay_inner.starts_with("Result<")
                            || pay_inner.starts_with("Map<")
                            || pay_inner.starts_with("Set<");
                        if !is_named_record
                            && !pay_is_string
                            && !pay_is_product_tuple
                            && !pay_is_container
                            && pay_sv_opt.is_none()
                        {
                            // fall through to i64 path
                        } else {
                            let pay_json = if is_named_record {
                                self.compile_record_to_json_cstr(&pay_inner, pay_rec_ptr)?
                            } else if pay_is_string {
                                let sj = self.emit_heap_string_payload_json(
                                    pay_sv_opt.ok_or_else(|| {
                                        CompileError::Generic(
                                            "to_json Result Option: string payload missing struct"
                                                .into(),
                                        )
                                    })?,
                                )?;
                                self.register_heap_alloc(sj);
                                sj
                            } else if pay_is_container {
                                // Nested container inside the Ok Option
                                // (`Option<List<…>>` / `Option<Result<…>>` / …):
                                // `pay_rec_ptr` already points at the inner
                                // container payload (the `List`/`Result` value),
                                // so dispatch `to_json` on that inner container
                                // directly — exactly as a bare `to_json(container)`
                                // would. The surrounding `{"Some":[…]}` wrapper is
                                // added by the Option path below, matching the
                                // bytecode VM.
                                let nested_val = BasicMetadataValueEnum::PointerValue(pay_rec_ptr);
                                match self.emit_typed_to_json_dispatch(&pay_inner, nested_val, None)? {
                                    Some(j) => match j {
                                        BasicValueEnum::PointerValue(p) => p,
                                        BasicValueEnum::StructValue(s) => self
                                            .build_extract_value(s.into(), 0, "res_opt_nested_ptr")?
                                            .into_pointer_value(),
                                        other => other.into_pointer_value(),
                                    },
                                    None => return Ok(None),
                                }
                            } else {
                                let sv = if let Some(s) = pay_sv_opt {
                                    s
                                } else {
                                    let rec_bty =
                                        self.llvm_type_for(&Type::Name(pay_inner.clone(), vec![]));
                                    let sty = match rec_bty {
                                        Some(BasicTypeEnum::StructType(s)) => s,
                                        _ => {
                                            return Err(CompileError::LlvmError(format!(
                                            "to_json: cannot resolve Result Option tuple type {}",
                                            pay_inner
                                        )))
                                        }
                                    };
                                    self.build_load(
                                        BasicTypeEnum::StructType(sty),
                                        pay_rec_ptr,
                                        "res_opt_tup_ld",
                                    )?
                                    .into_struct_value()
                                };
                                self.emit_product_tuple_to_json(sv)?
                            };
                            let disc_is_ok = self
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    disc_i64,
                                    self.context.i64_type().const_int(0, false),
                                    "res_opt_tup_is_ok",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let o_is_some = self
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    o_disc_i64,
                                    self.context.i64_type().const_int(0, false),
                                    "res_opt_tup_is_some",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let function = self.current_function().ok_or("no function")?;
                            let ok_bb = self
                                .context
                                .append_basic_block(function, "toj_res_opt_tup_ok");
                            let err_bb = self
                                .context
                                .append_basic_block(function, "toj_res_opt_tup_err");
                            let merge_bb = self
                                .context
                                .append_basic_block(function, "toj_res_opt_tup_merge");
                            let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                            let out_alloca = self.build_alloca(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                "toj_res_opt_tup_out",
                            )?;
                            self.builder
                                .build_conditional_branch(disc_is_ok, ok_bb, err_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(ok_bb);
                            let some_bb = self
                                .context
                                .append_basic_block(function, "toj_res_opt_tup_some");
                            let none_bb = self
                                .context
                                .append_basic_block(function, "toj_res_opt_tup_none");
                            let ok_merge = self
                                .context
                                .append_basic_block(function, "toj_res_opt_tup_ok_m");
                            self.builder
                                .build_conditional_branch(o_is_some, some_bb, none_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(some_bb);
                            let inner_buf = self.malloc_or_abort(
                                self.context.i64_type().const_int(1024, false),
                                "res_opt_tup_inner",
                            )?;
                            let ifmt = self
                                .builder
                                .build_global_string_ptr("{\"Some\":[%s]}", "res_opt_tup_ifmt")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let snprintf_fn = self.get_runtime_fn("snprintf")?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(inner_buf),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(1024, false),
                                    ),
                                    BasicMetadataValueEnum::PointerValue(ifmt.as_pointer_value()),
                                    BasicMetadataValueEnum::PointerValue(pay_json),
                                ],
                                "res_opt_tup_isn",
                            )?;
                            let outer_buf = self.malloc_or_abort(
                                self.context.i64_type().const_int(1024, false),
                                "res_opt_tup_outer",
                            )?;
                            let ofmt = self
                                .builder
                                .build_global_string_ptr("{\"Ok\":[%s]}", "res_opt_tup_ofmt")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(outer_buf),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(1024, false),
                                    ),
                                    BasicMetadataValueEnum::PointerValue(ofmt.as_pointer_value()),
                                    BasicMetadataValueEnum::PointerValue(inner_buf),
                                ],
                                "res_opt_tup_osn",
                            )?;
                            self.build_store(out_alloca, outer_buf)?;
                            self.builder
                                .build_unconditional_branch(ok_merge)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(none_bb);
                            let none_wrap = self.malloc_or_abort(
                                self.context.i64_type().const_int(32, false),
                                "res_opt_tup_none",
                            )?;
                            let nfmt = self
                                .builder
                                .build_global_string_ptr("{\"Ok\":[\"None\"]}", "res_opt_tup_nfmt")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let strcpy_fn = self.get_runtime_fn("strcpy")?;
                            self.build_call(
                                strcpy_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(none_wrap),
                                    BasicMetadataValueEnum::PointerValue(nfmt.as_pointer_value()),
                                ],
                                "res_opt_tup_ncpy",
                            )?;
                            self.build_store(out_alloca, none_wrap)?;
                            self.builder
                                .build_unconditional_branch(ok_merge)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(ok_merge);
                            self.builder
                                .build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(err_bb);
                            let ebuf = self.malloc_or_abort(
                                self.context.i64_type().const_int(32, false),
                                "res_opt_tup_err",
                            )?;
                            let efmt = self
                                .builder
                                .build_global_string_ptr("{\"Err\":[0]}", "res_opt_tup_efmt")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_call(
                                strcpy_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(ebuf),
                                    BasicMetadataValueEnum::PointerValue(efmt.as_pointer_value()),
                                ],
                                "res_opt_tup_ecpy",
                            )?;
                            self.build_store(out_alloca, ebuf)?;
                            self.builder
                                .build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(merge_bb);
                            let raw = self
                                .build_load(
                                    BasicTypeEnum::PointerType(i8_ptr_ty),
                                    out_alloca,
                                    "res_opt_tup_result",
                                )?
                                .into_pointer_value();
                            self.register_heap_alloc(raw);
                            return Ok(Some(self.wrap_c_string(raw)?));
                        } // else !is_named_record && pay_fields.len() < 2
                    }
                }
                let o_pay = match o_pay_bv {
                    BasicValueEnum::IntValue(iv) => iv,
                    BasicValueEnum::PointerValue(pv) => self
                        .builder
                        .build_ptr_to_int(pv, self.context.i64_type(), "res_opt_pay_ptr")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                    BasicValueEnum::StructValue(_) => {
                        // Nested Option/List heap-packed as struct — already
                        // handled above for multi-field product; treat as 0.
                        self.context.i64_type().const_int(0, false)
                    }
                    other => {
                        return Err(CompileError::Generic(format!(
                            "to_json Result Option: unexpected pay {:?}",
                            other.get_type()
                        )));
                    }
                };
                let o_pay_i64 = if o_pay.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(o_pay, self.context.i64_type(), "res_opt_pay_i64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    o_pay
                };
                // Nested Option of Map/Set/List needs typed helpers.
                let opt_json = if obj_type.contains("Map<") {
                    let mode = if obj_type.contains("Map<string, string>") {
                        1i64
                    } else if obj_type.contains("Map<string, bool>") {
                        2
                    } else if obj_type.contains("Map<string, f64>")
                        || obj_type.contains("Map<string, f32>")
                    {
                        3
                    } else {
                        self.map_nested_product_mode(&obj_type)
                    };
                    let opt_fn = self.get_runtime_fn("mimi_option_map_to_json")?;
                    self.build_call(
                        opt_fn,
                        &[
                            BasicMetadataValueEnum::IntValue(o_disc_i64),
                            BasicMetadataValueEnum::IntValue(o_pay_i64),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "res_opt_map_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("option map to_json void")?
                    .into_pointer_value()
                } else if obj_type.contains("Set<") {
                    let mode = if obj_type.contains("Set<string>") {
                        1i64
                    } else if obj_type.contains("Set<bool>") {
                        2
                    } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>") {
                        3
                    } else if let Some(elem) = obj_type
                        .find("Set<")
                        .map(|i| &obj_type[i + 4..])
                        .and_then(|s| {
                            let mut depth = 0i32;
                            for (j, ch) in s.char_indices() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' if depth == 0 => {
                                        return Some(s[..j].trim());
                                    }
                                    '>' | ')' => depth -= 1,
                                    _ => {}
                                }
                            }
                            None
                        })
                    {
                        if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                            let resolved = if self.is_product_tuple_alias(elem) {
                                self.resolve_alias_type_name(elem)
                            } else {
                                elem.to_string()
                            };
                            let mut arity: i64 = 0;
                            let mut depth = 0i32;
                            let mut any = false;
                            let body = resolved
                                .strip_prefix('(')
                                .and_then(|s| s.strip_suffix(')'))
                                .unwrap_or(resolved.as_str());
                            for ch in body.chars() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        arity += 1;
                                        any = true;
                                    }
                                    c if !c.is_whitespace() => any = true,
                                    _ => {}
                                }
                            }
                            if any {
                                arity += 1;
                            }
                            10 + arity.max(1)
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let opt_fn = self.get_runtime_fn("mimi_option_set_to_json")?;
                    self.build_call(
                        opt_fn,
                        &[
                            BasicMetadataValueEnum::IntValue(o_disc_i64),
                            BasicMetadataValueEnum::IntValue(o_pay_i64),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "res_opt_set_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("option set to_json void")?
                    .into_pointer_value()
                } else if obj_type.contains("List<") {
                    // Option of List: rebuild {"Some":[list_json]} / "None".
                    let disc_is_some = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            o_disc_i64,
                            self.context.i64_type().const_int(0, false),
                            "res_opt_list_some",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let function = self.current_function().ok_or("no function")?;
                    let some_bb = self
                        .context
                        .append_basic_block(function, "toj_res_opt_list_some");
                    let none_bb = self
                        .context
                        .append_basic_block(function, "toj_res_opt_list_none");
                    let merge_bb = self
                        .context
                        .append_basic_block(function, "toj_res_opt_list_merge");
                    let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let out_alloca = self.build_alloca(
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        "toj_res_opt_list_out",
                    )?;
                    self.builder
                        .build_conditional_branch(disc_is_some, some_bb, none_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(some_bb);
                    let list_ptr = self
                        .builder
                        .build_int_to_ptr(o_pay_i64, i8_ptr_ty, "res_opt_list_ptr")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    // Result<Option<List<(…)>>> product path.
                    let list_elem = Self::strip_first_type_arg(&obj_type, "Result")
                        .and_then(|s| Self::strip_first_type_arg(&s, "Option"))
                        .and_then(|s| {
                            s.strip_prefix("List<")
                                .and_then(|x| x.strip_suffix('>'))
                                .map(|x| x.to_string())
                        })
                        .unwrap_or_default();
                    let list_json = if list_elem.starts_with('(')
                        || self.is_product_tuple_alias(&list_elem)
                    {
                        let elem = if self.is_product_tuple_alias(&list_elem) {
                            self.resolve_alias_type_name(&list_elem)
                        } else {
                            list_elem
                        };
                        self.emit_list_product_tuple_to_json(list_ptr, &elem)?
                    } else {
                        let list_fn_name = if obj_type.contains("List<Map") {
                            if obj_type.contains("Map<string, string>") {
                                "mimi_list_map_to_json_string"
                            } else {
                                "mimi_list_map_to_string"
                            }
                        } else if obj_type.contains("List<string>") {
                            "mimi_list_str_to_json"
                        } else if obj_type.contains("List<f64>") || obj_type.contains("List<f32>") {
                            "mimi_list_f64_to_json"
                        } else if obj_type.contains("List<bool>") {
                            "mimi_list_bool_to_json"
                        } else {
                            "mimi_list_i64_to_json"
                        };
                        let list_fn_ty = i8_ptr_ty
                            .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
                        let list_fn = self.module.get_function(list_fn_name).unwrap_or_else(|| {
                            self.module.add_function(
                                list_fn_name,
                                list_fn_ty,
                                Some(inkwell::module::Linkage::External),
                            )
                        });
                        self.build_call(
                            list_fn,
                            &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                            "res_opt_list_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list to_json void")?
                        .into_pointer_value()
                    };
                    let buf = self.malloc_or_abort(
                        self.context.i64_type().const_int(4096, false),
                        "res_opt_list_buf",
                    )?;
                    let fmt = self
                        .builder
                        .build_global_string_ptr("{\"Some\":[%s]}", "res_opt_list_fmt")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let snprintf_fn = self.get_runtime_fn("snprintf")?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(4096, false),
                            ),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(list_json),
                        ],
                        "res_opt_list_sn",
                    )?;
                    self.build_store(out_alloca, buf)?;
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(none_bb);
                    let none_heap = self.malloc_or_abort(
                        self.context.i64_type().const_int(8, false),
                        "res_opt_list_none",
                    )?;
                    let none_lit = self
                        .builder
                        .build_global_string_ptr("\"None\"", "res_opt_list_none_lit")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let strcpy_fn = self.get_runtime_fn("strcpy")?;
                    self.build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(none_heap),
                            BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                        ],
                        "res_opt_list_none_cpy",
                    )?;
                    self.build_store(out_alloca, none_heap)?;
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(merge_bb);
                    self.build_load(
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        out_alloca,
                        "res_opt_list_result",
                    )?
                    .into_pointer_value()
                } else {
                    let opt_fn = self.get_runtime_fn("mimi_option_i64_to_json")?;
                    self.build_call(
                        opt_fn,
                        &[
                            BasicMetadataValueEnum::IntValue(o_disc_i64),
                            BasicMetadataValueEnum::IntValue(o_pay_i64),
                        ],
                        "res_opt_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("option to_json void")?
                    .into_pointer_value()
                };
                let disc_is_ok = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        disc_i64,
                        self.context.i64_type().const_int(0, false),
                        "res_opt_is_ok",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let function = self.current_function().ok_or("no function")?;
                let ok_bb = self.context.append_basic_block(function, "toj_res_opt_ok");
                let err_bb = self.context.append_basic_block(function, "toj_res_opt_err");
                let merge_bb = self
                    .context
                    .append_basic_block(function, "toj_res_opt_merge");
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let out_alloca =
                    self.build_alloca(BasicTypeEnum::PointerType(i8_ptr_ty), "toj_res_opt_out")?;
                self.builder
                    .build_conditional_branch(disc_is_ok, ok_bb, err_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(ok_bb);
                let buf = self.malloc_or_abort(
                    self.context.i64_type().const_int(512, false),
                    "res_opt_buf",
                )?;
                let fmt = self
                    .builder
                    .build_global_string_ptr("{\"Ok\":[%s]}", "res_opt_fmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(
                            self.context.i64_type().const_int(512, false),
                        ),
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(opt_json),
                    ],
                    "res_opt_sn",
                )?;
                self.build_store(out_alloca, buf)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(err_bb);
                let err_bv = self.build_extract_value(sv.into(), 2, "res_opt_err")?;
                let err_i64 = match err_bv {
                    BasicValueEnum::IntValue(iv) => {
                        if iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(iv, self.context.i64_type(), "res_opt_err_i64")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            iv
                        }
                    }
                    _ => self.context.i64_type().const_int(0, false),
                };
                let ebuf = self.malloc_or_abort(
                    self.context.i64_type().const_int(128, false),
                    "res_opt_ebuf",
                )?;
                let efmt = self
                    .builder
                    .build_global_string_ptr("{\"Err\":[%ld]}", "res_opt_efmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(ebuf),
                        BasicMetadataValueEnum::IntValue(
                            self.context.i64_type().const_int(128, false),
                        ),
                        BasicMetadataValueEnum::PointerValue(efmt.as_pointer_value()),
                        BasicMetadataValueEnum::IntValue(err_i64),
                    ],
                    "res_opt_esn",
                )?;
                self.build_store(out_alloca, ebuf)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let raw = self
                    .build_load(
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        out_alloca,
                        "res_opt_result",
                    )?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            // Result of List: Ok may be by-value list struct {i64,ptr}
            // or a pointer/int handle — handle before scalar ok_i64 coercion.
            // Result of List: Ok type must start with List (not Map/Set of List).
            let result_ok_is_list = obj_type
                .strip_prefix("Result<")
                .map(|s| {
                    let mut depth = 0i32;
                    for (i, ch) in s.char_indices() {
                        match ch {
                            '<' => depth += 1,
                            '>' => depth -= 1,
                            ',' if depth == 0 => {
                                return s[..i].trim().starts_with("List");
                            }
                            _ => {}
                        }
                    }
                    false
                })
                .unwrap_or(false);
            if result_ok_is_list {
                let err_bv = self.build_extract_value(sv.into(), 2, "res_list_err")?;
                let err_i64 = match err_bv {
                    BasicValueEnum::IntValue(iv) => {
                        if iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(iv, self.context.i64_type(), "res_list_err_i64")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            iv
                        }
                    }
                    BasicValueEnum::PointerValue(pv) => self
                        .builder
                        .build_ptr_to_int(pv, self.context.i64_type(), "res_list_err_ptr")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                    _ => self.context.i64_type().const_int(0, false),
                };
                let disc_is_ok = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        disc_i64,
                        self.context.i64_type().const_int(0, false),
                        "res_list_ok",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let function = self.current_function().ok_or("no function")?;
                let ok_bb = self.context.append_basic_block(function, "toj_res_list_ok");
                let err_bb = self
                    .context
                    .append_basic_block(function, "toj_res_list_err");
                let merge_bb = self
                    .context
                    .append_basic_block(function, "toj_res_list_merge");
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let out_alloca =
                    self.build_alloca(BasicTypeEnum::PointerType(i8_ptr_ty), "toj_res_list_out")?;
                self.builder
                    .build_conditional_branch(disc_is_ok, ok_bb, err_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(ok_bb);
                // Materialize list as a pointer for runtime helpers.
                let list_ptr = match ok_bv {
                    BasicValueEnum::StructValue(lsv) => {
                        let list_alloca = self.build_alloca(
                            BasicTypeEnum::StructType(lsv.get_type()),
                            "res_list_tmp",
                        )?;
                        self.build_store(list_alloca, lsv)?;
                        list_alloca
                    }
                    BasicValueEnum::PointerValue(pv) => pv,
                    BasicValueEnum::IntValue(iv) => {
                        let as_i64 = if iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(iv, self.context.i64_type(), "res_list_ok_i64")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            iv
                        };
                        self.builder
                            .build_int_to_ptr(as_i64, i8_ptr_ty, "res_list_as_ptr")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    }
                    other => {
                        return Err(CompileError::Generic(format!(
                            "to_json Result List: unexpected Ok payload {:?}",
                            other.get_type()
                        )));
                    }
                };
                // Product-tuple list elements need codegen helpers.
                // Use paren-aware strip: Result<List<(i32,i32)>,string> must
                // not split on the tuple comma.
                let list_elem =
                    crate::codegen::CodeGenerator::strip_first_type_arg(&obj_type, "Result")
                        .and_then(|s| {
                            s.strip_prefix("List<")
                                .and_then(|x| x.strip_suffix('>'))
                                .map(|x| x.to_string())
                        })
                        .unwrap_or_default();
                let list_json = if list_elem.starts_with('(')
                    || self.is_product_tuple_alias(&list_elem)
                {
                    let elem = if self.is_product_tuple_alias(&list_elem) {
                        self.resolve_alias_type_name(&list_elem)
                    } else {
                        list_elem
                    };
                    self.emit_list_product_tuple_to_json(list_ptr, &elem)?
                } else if let Some(opt_inner) = list_elem
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner) {
                        let elem = if self.is_product_tuple_alias(opt_inner) {
                            self.resolve_alias_type_name(opt_inner)
                        } else {
                            opt_inner.to_string()
                        };
                        let arity = {
                            let body = elem
                                .strip_prefix('(')
                                .and_then(|s| s.strip_suffix(')'))
                                .unwrap_or(&elem);
                            let mut arity = 0i64;
                            let mut depth = 0i32;
                            let mut any = false;
                            for ch in body.chars() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        arity += 1;
                                        any = true;
                                    }
                                    c if !c.is_whitespace() => any = true,
                                    _ => {}
                                }
                            }
                            if any {
                                arity += 1;
                            }
                            arity.max(1)
                        };
                        let func = self.get_runtime_fn("mimi_list_option_product_to_json")?;
                        let i64_ty = self.context.i64_type();
                        self.build_call(
                            func,
                            &[
                                BasicMetadataValueEnum::PointerValue(list_ptr),
                                BasicMetadataValueEnum::IntValue(
                                    i64_ty.const_int(arity as u64, false),
                                ),
                                BasicMetadataValueEnum::IntValue(i64_ty.const_int(0, false)),
                            ],
                            "res_list_opt_prod_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list option product to_json void")?
                        .into_pointer_value()
                    } else {
                        // fallback scalar option list
                        let list_fn = self.get_runtime_fn("mimi_list_i64_to_json")?;
                        self.build_call(
                            list_fn,
                            &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                            "res_list_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list to_json void")?
                        .into_pointer_value()
                    }
                } else if list_elem.starts_with("Map") {
                    if let Some(val_ty) = list_elem
                        .strip_prefix("Map<string, ")
                        .and_then(|s| s.strip_suffix('>'))
                        .or_else(|| {
                            list_elem
                                .strip_prefix("Map<string,")
                                .and_then(|s| s.strip_suffix('>'))
                                .map(|s| s.trim())
                        })
                    {
                        if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                            let elem = if self.is_product_tuple_alias(val_ty) {
                                self.resolve_alias_type_name(val_ty)
                            } else {
                                val_ty.to_string()
                            };
                            self.emit_list_map_product_to_json(list_ptr, &elem)?
                        } else {
                            let list_fn_name = if list_elem.contains("Map<string, string>") {
                                "mimi_list_map_to_json_string"
                            } else {
                                "mimi_list_map_to_string"
                            };
                            let list_fn = self.get_runtime_fn(list_fn_name)?;
                            self.build_call(
                                list_fn,
                                &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                                "res_list_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list map to_json void")?
                            .into_pointer_value()
                        }
                    } else {
                        let list_fn = self.get_runtime_fn("mimi_list_map_to_string")?;
                        self.build_call(
                            list_fn,
                            &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                            "res_list_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list map to_json void")?
                        .into_pointer_value()
                    }
                } else if list_elem.starts_with("Set") {
                    if let Some(elem) = list_elem
                        .strip_prefix("Set<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                            let resolved = if self.is_product_tuple_alias(elem) {
                                self.resolve_alias_type_name(elem)
                            } else {
                                elem.to_string()
                            };
                            let mut arity: i64 = 0;
                            let mut depth = 0i32;
                            let mut any = false;
                            let body = resolved
                                .strip_prefix('(')
                                .and_then(|s| s.strip_suffix(')'))
                                .unwrap_or(resolved.as_str());
                            for ch in body.chars() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        arity += 1;
                                        any = true;
                                    }
                                    c if !c.is_whitespace() => any = true,
                                    _ => {}
                                }
                            }
                            if any {
                                arity += 1;
                            }
                            let func = self.get_runtime_fn("mimi_list_set_product_to_json")?;
                            self.build_call(
                                func,
                                &[
                                    BasicMetadataValueEnum::PointerValue(list_ptr),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context
                                            .i64_type()
                                            .const_int(arity.max(1) as u64, false),
                                    ),
                                ],
                                "res_list_set_product_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list set product to_json void")?
                            .into_pointer_value()
                        } else {
                            let list_fn = self.get_runtime_fn("mimi_list_set_to_json")?;
                            self.build_call(
                                list_fn,
                                &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                                "res_list_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list set to_json void")?
                            .into_pointer_value()
                        }
                    } else {
                        let list_fn = self.get_runtime_fn("mimi_list_set_to_json")?;
                        self.build_call(
                            list_fn,
                            &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                            "res_list_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list set to_json void")?
                        .into_pointer_value()
                    }
                } else {
                    let list_fn_name =
                        if obj_type.contains("List<Map") || obj_type.contains("List<Map<") {
                            if obj_type.contains("Map<string, string>") {
                                "mimi_list_map_to_json_string"
                            } else {
                                "mimi_list_map_to_string"
                            }
                        } else if obj_type.contains("List<Set") {
                            "mimi_list_set_to_json"
                        } else if obj_type.contains("List<string>") {
                            "mimi_list_str_to_json"
                        } else if obj_type.contains("List<f64>") || obj_type.contains("List<f32>") {
                            "mimi_list_f64_to_json"
                        } else if obj_type.contains("List<bool>") {
                            "mimi_list_bool_to_json"
                        } else {
                            "mimi_list_i64_to_json"
                        };
                    let list_fn_ty =
                        i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
                    let list_fn = self.module.get_function(list_fn_name).unwrap_or_else(|| {
                        self.module.add_function(
                            list_fn_name,
                            list_fn_ty,
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                    self.build_call(
                        list_fn,
                        &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                        "res_list_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("list to_json void")?
                    .into_pointer_value()
                };
                let buf = self.malloc_or_abort(
                    self.context.i64_type().const_int(4096, false),
                    "res_list_buf",
                )?;
                let fmt = self
                    .builder
                    .build_global_string_ptr("{\"Ok\":[%s]}", "res_list_fmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(
                            self.context.i64_type().const_int(4096, false),
                        ),
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(list_json),
                    ],
                    "res_list_sn",
                )?;
                self.build_store(out_alloca, buf)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(err_bb);
                let ebuf = self.emit_result_err_json(err_i64, true)?;
                self.build_store(out_alloca, ebuf)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let raw = self
                    .build_load(
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        out_alloca,
                        "res_list_result",
                    )?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            // Result of product tuple / named record / heap-string Ok. The Ok
            // payload may be a struct value OR (for `Result<record>` /
            // `Result<product-tuple>`) a *pointer* to the data — normalize to a
            // *record pointer* (no eager load) so records stored by pointer
            // serialize recursively like the bytecode VM (`{"Ok":[{…}]}`).
            let (ok_rec_ptr, ok_sv_opt, ok_skip) = match ok_bv {
                BasicValueEnum::StructValue(sv) => {
                    let rec_ty = sv.get_type();
                    let rec_alloca =
                        self.build_alloca(BasicTypeEnum::StructType(rec_ty), "res_rec_tmp")?;
                    self.build_store(rec_alloca, sv)?;
                    let fields = rec_ty.get_field_types();
                    let ok_is_string = fields.len() == 2
                        && matches!(fields[0], BasicTypeEnum::PointerType(_))
                        && matches!(
                            fields[1],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                        );
                    // Nested Option/Result Ok payloads start with i1 — not product tuples.
                    let ok_is_nested = !fields.is_empty()
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 1
                        );
                    let ok_is_list = fields.len() == 2
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                        )
                        && matches!(fields[1], BasicTypeEnum::PointerType(_));
                    (Some(rec_alloca), Some(sv), ok_is_string)
                }
                BasicValueEnum::PointerValue(pv) => {
                    // `Result<record>` stores the record by pointer (`ptr` to the
                    // record data); use it directly (no deref, no eager load).
                    (Some(pv), None, false)
                }
                _ => (None, None, true),
            };
            if let Some(ok_rec_ptr) = ok_rec_ptr {
                if !ok_skip {
                    let mut ok_inner = Self::extract_result_ok_type(&obj_type);
                    if ok_inner.is_empty() {
                        if let Some(sv) = ok_sv_opt {
                            let pay_sty = sv.get_type();
                            for (n, ty) in &self.type_llvm {
                                if matches!(ty, BasicTypeEnum::StructType(s) if *s == pay_sty)
                                    && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                    })
                                {
                                    ok_inner = n.clone();
                                    break;
                                }
                            }
                        } else if !self.type_llvm.is_empty() {
                            // bare-variant / unrecoverable Ok: first registered
                            // record (only used when the name is never dereferenced).
                            for (n, ty) in &self.type_llvm {
                                if matches!(ty, BasicTypeEnum::StructType(_))
                                    && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                    })
                                {
                                    ok_inner = n.clone();
                                    break;
                                }
                            }
                        }
                    }
                    let is_named_record = self
                        .type_defs
                        .get(&ok_inner)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)));
                    let ok_is_string = ok_sv_opt.is_some_and(|sv| {
                        let f = sv.get_type().get_field_types();
                        f.len() == 2
                            && matches!(f[0], BasicTypeEnum::PointerType(_))
                            && matches!(f[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64)
                    });
                    let ok_is_product_tuple =
                        ok_inner.starts_with('(') || self.is_product_tuple_alias(&ok_inner);
                    let ok_is_container = ok_inner.starts_with("Option<")
                        || ok_inner.starts_with("List<")
                        || ok_inner.starts_with("Result<")
                        || ok_inner.starts_with("Map<")
                        || ok_inner.starts_with("Set<");
                    let ok_fields_len = ok_sv_opt.map(|sv| sv.get_type().get_field_types().len());
                    // Fall through to the scalar i64 path for single-field
                    // non-record/non-string Ok payloads (matches prior behavior).
                    if !is_named_record
                        && !ok_is_string
                        && !ok_is_product_tuple
                        && !ok_is_container
                        && ok_fields_len.map_or(true, |l| l < 2)
                    {
                        // fall through
                    } else {
                        let ok_json = if is_named_record {
                            self.compile_record_to_json_cstr(&ok_inner, ok_rec_ptr)?
                        } else if ok_is_string {
                            // D-3: heap-string Ok payload {ptr,i64} must
                            // NOT be treated as a 2-field product tuple
                            // (its 2 struct fields would otherwise
                            // mis-serialize as [ptr,len]). Emit a JSON
                            // string literal instead.
                            let sj = self.emit_heap_string_payload_json(ok_sv_opt.ok_or_else(
                                || {
                                    CompileError::Generic(
                                        "to_json Result: Ok string payload missing struct".into(),
                                    )
                                },
                            )?)?;
                            self.register_heap_alloc(sj);
                            sj
                        } else if ok_is_container {
                            // Nested container Ok payload (`Option<…>` / `List<…>` /
                            // `Result<…>` / `Map<…>` / `Set<…>`): dispatch to_json
                            // on the inner value with its own type name, so nested
                            // containers serialize exactly like the bytecode VM.
                            let nested_val = BasicMetadataValueEnum::PointerValue(ok_rec_ptr);
                            match self.emit_typed_to_json_dispatch(&ok_inner, nested_val, None)? {
                                Some(j) => match j {
                                    BasicValueEnum::PointerValue(p) => p,
                                    BasicValueEnum::StructValue(s) => self
                                        .build_extract_value(s.into(), 0, "res_ok_nested_ptr")?
                                        .into_pointer_value(),
                                    other => other.into_pointer_value(),
                                },
                                None => return Ok(None),
                            }
                        } else {
                            let sv = if let Some(s) = ok_sv_opt {
                                s
                            } else {
                                let rec_bty =
                                    self.llvm_type_for(&Type::Name(ok_inner.clone(), vec![]));
                                let sty = match rec_bty {
                                    Some(BasicTypeEnum::StructType(s)) => s,
                                    _ => {
                                        return Err(CompileError::LlvmError(format!(
                                            "to_json: cannot resolve Result tuple type {}",
                                            ok_inner
                                        )))
                                    }
                                };
                                self.build_load(
                                    BasicTypeEnum::StructType(sty),
                                    ok_rec_ptr,
                                    "res_tup_ld",
                                )?
                                .into_struct_value()
                            };
                            self.emit_product_tuple_to_json(sv)?
                        };
                        let err_bv = self.build_extract_value(sv.into(), 2, "res_err_tup")?;
                        let err_i64 = match err_bv {
                            BasicValueEnum::IntValue(iv) => {
                                if iv.get_type().get_bit_width() < 64 {
                                    self.builder
                                        .build_int_s_extend(
                                            iv,
                                            self.context.i64_type(),
                                            "res_err_tup_i64",
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                } else {
                                    iv
                                }
                            }
                            BasicValueEnum::PointerValue(pv) => self
                                .builder
                                .build_ptr_to_int(pv, self.context.i64_type(), "res_err_tup_ptr")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                            _ => self.context.i64_type().const_int(0, false),
                        };
                        // Result disc: true/1 = Ok, false/0 = Err (matches mimi_result_*_to_json).
                        let disc_is_ok = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                disc_i64,
                                self.context.i64_type().const_int(0, false),
                                "res_tup_is_ok",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let function = self.current_function().ok_or("no function")?;
                        let ok_bb = self.context.append_basic_block(function, "toj_res_tup_ok");
                        let err_bb = self.context.append_basic_block(function, "toj_res_tup_err");
                        let merge_bb = self
                            .context
                            .append_basic_block(function, "toj_res_tup_merge");
                        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let out_alloca = self.build_alloca(
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            "toj_res_tup_out",
                        )?;
                        self.builder
                            .build_conditional_branch(disc_is_ok, ok_bb, err_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(ok_bb);
                        // D-3: sized assembly instead of the old fixed
                        // 1024-byte snprintf — long payloads truncated
                        // at 1023 (NUL) on the native backend while the
                        // VM kept the full rendering.
                        let wrap = self.sized_cat_parts(
                            &[
                                crate::codegen::builtins::io::CatPart::Lit("{\"Ok\":["),
                                crate::codegen::builtins::io::CatPart::Dyn(ok_json),
                                crate::codegen::builtins::io::CatPart::Lit("]}"),
                            ],
                            "res_tup_ok_wrap",
                            false,
                        )?;
                        self.build_store(out_alloca, wrap)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(err_bb);
                        let ebuf = self.emit_result_err_json(err_i64, true)?;
                        self.build_store(out_alloca, ebuf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(merge_bb);
                        let raw = self
                            .build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "res_tup_result",
                            )?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return Ok(Some(self.wrap_c_string(raw)?));
                    } // else is_named_record || multi-field
                }
            }
            let ok_i64 = match ok_bv {
                BasicValueEnum::IntValue(iv) => {
                    if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(iv, self.context.i64_type(), "res_ok_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    }
                }
                BasicValueEnum::PointerValue(pv) => self
                    .builder
                    .build_ptr_to_int(pv, self.context.i64_type(), "res_ok_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                BasicValueEnum::StructValue(sv) => {
                    // D-3: heap-string Ok payload {ptr,i64} — serialize
                    // to a JSON string literal (the 5908 arm already
                    // excluded strings from the product-tuple path;
                    // without this the payload hits the generic E0700).
                    let j = self.emit_heap_string_payload_json(sv)?;
                    self.register_heap_alloc(j);
                    self.builder
                        .build_ptr_to_int(j, self.context.i64_type(), "res_ok_str_json")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                }
                other => {
                    return Err(CompileError::Generic(format!(
                        "to_json Result: unexpected Ok payload {:?}",
                        other.get_type()
                    )));
                }
            };
            let err_bv = self.build_extract_value(sv.into(), 2, "res_err")?;
            let err_i64 = match err_bv {
                BasicValueEnum::IntValue(iv) => {
                    if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(iv, self.context.i64_type(), "res_err_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    }
                }
                BasicValueEnum::PointerValue(pv) => self
                    .builder
                    .build_ptr_to_int(pv, self.context.i64_type(), "res_err_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                BasicValueEnum::StructValue(sv) => {
                    // D-3: heap-string Err payload → JSON string literal.
                    let j = self.emit_heap_string_payload_json(sv)?;
                    self.register_heap_alloc(j);
                    self.builder
                        .build_ptr_to_int(j, self.context.i64_type(), "res_err_str_json")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                }
                other => {
                    return Err(CompileError::Generic(format!(
                        "to_json Result: unexpected Err payload {:?}",
                        other.get_type()
                    )));
                }
            };
            let ok_root = obj_type
                .strip_prefix("Result<")
                .and_then(|s| {
                    let mut depth = 0i32;
                    for (i, ch) in s.char_indices() {
                        match ch {
                            '<' => depth += 1,
                            '>' => depth -= 1,
                            ',' if depth == 0 => {
                                return Some(s[..i].trim());
                            }
                            _ => {}
                        }
                    }
                    None
                })
                .unwrap_or(obj_type.as_str());
            if ok_root.starts_with("Set<") {
                let mode = if obj_type.contains("Set<string>") {
                    1i64
                } else if obj_type.contains("Set<bool>") {
                    2
                } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>") {
                    3
                } else if let Some(elem) = obj_type
                    .strip_prefix("Result<")
                    .and_then(|s| {
                        let mut depth = 0i32;
                        for (i, ch) in s.char_indices() {
                            match ch {
                                '<' => depth += 1,
                                '>' => depth -= 1,
                                ',' if depth == 0 => {
                                    return Some(s[..i].trim());
                                }
                                _ => {}
                            }
                        }
                        None
                    })
                    .and_then(|s| s.strip_prefix("Set<"))
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                        let resolved = if self.is_product_tuple_alias(elem) {
                            self.resolve_alias_type_name(elem)
                        } else {
                            elem.to_string()
                        };
                        let mut arity: i64 = 0;
                        let mut depth = 0i32;
                        let mut any = false;
                        let body = resolved
                            .strip_prefix('(')
                            .and_then(|s| s.strip_suffix(')'))
                            .unwrap_or(resolved.as_str());
                        for ch in body.chars() {
                            match ch {
                                '<' | '(' => depth += 1,
                                '>' | ')' => depth -= 1,
                                ',' if depth == 0 => {
                                    arity += 1;
                                    any = true;
                                }
                                c if !c.is_whitespace() => any = true,
                                _ => {}
                            }
                        }
                        if any {
                            arity += 1;
                        }
                        10 + arity.max(1)
                    } else if let Some(opt_inner) = elem
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner) {
                            let resolved = if self.is_product_tuple_alias(opt_inner) {
                                self.resolve_alias_type_name(opt_inner)
                            } else {
                                opt_inner.to_string()
                            };
                            let mut arity: i64 = 0;
                            let mut depth = 0i32;
                            let mut any = false;
                            let body = resolved
                                .strip_prefix('(')
                                .and_then(|s| s.strip_suffix(')'))
                                .unwrap_or(resolved.as_str());
                            for ch in body.chars() {
                                match ch {
                                    '<' | '(' => depth += 1,
                                    '>' | ')' => depth -= 1,
                                    ',' if depth == 0 => {
                                        arity += 1;
                                        any = true;
                                    }
                                    c if !c.is_whitespace() => any = true,
                                    _ => {}
                                }
                            }
                            if any {
                                arity += 1;
                            }
                            50 + arity.max(1)
                        } else {
                            0
                        }
                    } else if elem.starts_with("Map<string, ") {
                        if let Some(val_ty) = elem
                            .strip_prefix("Map<string, ")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                let resolved = if self.is_product_tuple_alias(val_ty) {
                                    self.resolve_alias_type_name(val_ty)
                                } else {
                                    val_ty.to_string()
                                };
                                let mut arity: i64 = 0;
                                let mut depth = 0i32;
                                let mut any = false;
                                let body = resolved
                                    .strip_prefix('(')
                                    .and_then(|s| s.strip_suffix(')'))
                                    .unwrap_or(resolved.as_str());
                                for ch in body.chars() {
                                    match ch {
                                        '<' | '(' => depth += 1,
                                        '>' | ')' => depth -= 1,
                                        ',' if depth == 0 => {
                                            arity += 1;
                                            any = true;
                                        }
                                        c if !c.is_whitespace() => any = true,
                                        _ => {}
                                    }
                                }
                                if any {
                                    arity += 1;
                                }
                                70 + arity.max(1)
                            } else {
                                0
                            }
                        } else {
                            0
                        }
                    } else {
                        0
                    }
                } else {
                    0
                };
                let func = self.get_runtime_fn("mimi_result_set_to_json")?;
                let raw = self
                    .build_call(
                        func,
                        &[
                            BasicMetadataValueEnum::IntValue(disc_i64),
                            BasicMetadataValueEnum::IntValue(ok_i64),
                            BasicMetadataValueEnum::IntValue(err_i64),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "to_json_res_set",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_result_set_to_json void")?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            if ok_root.starts_with("Map<") {
                let mode = if obj_type.contains("Map<string, string>") {
                    1i64
                } else if obj_type.contains("Map<string, bool>") {
                    2
                } else if obj_type.contains("Map<string, f64>")
                    || obj_type.contains("Map<string, f32>")
                {
                    3
                } else {
                    self.map_nested_product_mode(&obj_type)
                };
                let func = self.get_runtime_fn("mimi_result_map_to_json")?;
                let raw = self
                    .build_call(
                        func,
                        &[
                            BasicMetadataValueEnum::IntValue(disc_i64),
                            BasicMetadataValueEnum::IntValue(ok_i64),
                            BasicMetadataValueEnum::IntValue(err_i64),
                            BasicMetadataValueEnum::IntValue(
                                self.context.i64_type().const_int(mode as u64, false),
                            ),
                        ],
                        "to_json_res_map",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_result_map_to_json void")?
                    .into_pointer_value();
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            // Prefer structured Result JSON so string Err (heap {ptr,len}) is
            // not printed as a raw i64 address.
            if let BasicMetadataValueEnum::StructValue(res_sv) = arg0 {
                let raw = self.emit_result_struct_to_json_cstr(res_sv, &obj_type)?;
                self.register_heap_alloc(raw);
                return Ok(Some(self.wrap_c_string(raw)?));
            }
            let func = self.get_runtime_fn("mimi_result_i64_to_json")?;
            let raw = self
                .build_call(
                    func,
                    &[
                        BasicMetadataValueEnum::IntValue(disc_i64),
                        BasicMetadataValueEnum::IntValue(ok_i64),
                        BasicMetadataValueEnum::IntValue(err_i64),
                    ],
                    "to_json_res",
                )?
                .try_as_basic_value_opt()
                .ok_or("mimi_result_i64_to_json void")?
                .into_pointer_value();
            self.register_heap_alloc(raw);
            return Ok(Some(self.wrap_c_string(raw)?));
        }
        // Check for Record type — serialize to JSON object via sprintf
        if self
            .type_defs
            .get(&obj_type)
            .is_some_and(|td| matches!(td.kind, TypeDefKind::Record(_)))
        {
            // Shape normalization (0.39.136 architecture): the legacy emitter
            // surfaces named records as pointers (alloca-backed), the resolved
            // emitter as bare struct values. Normalize here so both callers
            // share one path.
            let struct_ptr = match &arg0 {
                BasicMetadataValueEnum::PointerValue(pv) => *pv,
                BasicMetadataValueEnum::StructValue(sv) => {
                    let sv_ty = sv.get_type();
                    let alloca = self
                        .build_alloca(BasicTypeEnum::StructType(sv_ty), "to_json_rec_val_alloca")?;
                    self.build_store(alloca, *sv)?;
                    alloca
                }
                _ => {
                    return Err(CompileError::Generic(
                        "to_json: record value must be a pointer".into(),
                    ))
                }
            };
            let raw = self.compile_record_to_json_cstr(&obj_type, struct_ptr)?;
            self.register_heap_alloc(raw);
            return Ok(Some(self.wrap_c_string(raw)?));
        }

        Ok(None)
    }

    pub(in crate::codegen) fn compile_call_expr(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match callee.unlocated() {
            Expr::Ident(name) => {
                match name.as_str() {
                    "type_name" | "type_fields" | "type_variants" | "keys" | "values" | "map"
                    | "filter" | "reduce" => {
                        return self.compile_builtin_intrinsic(name, args, vars);
                    }
                    // 条款 11 escape hatch: unsafe_cast_protocol(x) is an
                    // identity cast — the value passes through unchanged; the
                    // dyn fat-pointer packing is handled by the dyn let-binding
                    // site in codegen/func.rs.
                    "unsafe_cast_protocol" => {
                        if args.len() == 1 {
                            return self.compile_expr(&args[0], vars);
                        }
                    }
                    _ => {}
                }

                if self.fn_ptr_var_names.contains(name.as_str()) {
                    if let Some(&(alloca, ty)) = vars.get(name.as_str()) {
                        return self.compile_fn_ptr_var_call(name, alloca, ty, args, vars);
                    }
                }

                if let Some(&(alloca, BasicTypeEnum::StructType(st))) = vars.get(name.as_str()) {
                    if st.get_field_types().len() == 2 {
                        let closure_val = self.build_load(
                            BasicTypeEnum::StructType(st),
                            alloca,
                            &format!("{}_closure", name),
                        )?;
                        let compiled_args = self.compile_arg_values(args, vars)?;
                        let ret_ty = self
                            .var_types
                            .get(name.as_str())
                            .and_then(|ty| Self::closure_return_llvm_type(self, ty));
                        let result =
                            self.compile_closure_call(closure_val, &compiled_args, ret_ty)?;
                        // L6: when the closure returns a custom enum, register its
                        // payload box for a tag-conditional free at this (caller)
                        // scope exit. The lambda claimed the box on return
                        // (claim_returned_enum_box) — ownership transfers here,
                        // mirroring named-function calls. The closure's declared
                        // return type comes from its Func type in var_types.
                        let closure_ret_ast =
                            self.var_types
                                .get(name.as_str())
                                .and_then(|ty| match ty.unlocated() {
                                    Type::Func(_, ret) | Type::ExternFunc(_, ret) => {
                                        Some(ret.as_ref().clone())
                                    }
                                    _ => None,
                                });
                        return self.register_enum_box_for_return(closure_ret_ast.as_ref(), result);
                    }
                }

                // 0.36.4 Fault nominal (裁决 1): a bare state/event name in call
                // position that is a StateId/EventId variant compiles to a nominal
                // variant value (build_nominal_variant). Panic { code } carries a
                // single string payload; other variants are no-payload.
                // 0.37.x: flow EventId/StateId bare-variant names must only be
                // recognized while compiling a flow transition body. The old
                // unscoped fallback made user transition names (e.g. `accept`)
                // shadow same-named builtins in later plain functions, because
                // every flow transition is also an EventId enum variant.
                if !self.current_flow_name.is_empty() {
                    if let Some(enum_type) = self.nominal_variant_enum(name.as_str()) {
                        let payload = if args.len() == 1 {
                            match args[0].unlocated() {
                                Expr::Literal(Lit::String(s)) => Some(s.clone()),
                                _ => None,
                            }
                        } else {
                            None
                        };
                        return self.build_nominal_variant(&enum_type, name, payload.as_deref());
                    }
                }

                self.compile_call(name, args, vars)
            }
            Expr::Field(obj, method_name) => {
                if let Expr::Ident(type_name) = obj.unlocated() {
                    let is_builtin_enum = type_name == "Result" || type_name == "Option";
                    let is_custom_enum = self
                        .type_defs
                        .get(type_name)
                        .map(|td| matches!(td.kind, crate::ast::TypeDefKind::Enum(_)))
                        .unwrap_or(false);
                    if is_builtin_enum {
                        return self.compile_call(method_name, args, vars);
                    }
                    if is_custom_enum {
                        return self.compile_custom_enum_constructor_call(
                            type_name,
                            method_name,
                            args,
                            vars,
                        );
                    }
                }
                self.compile_method_call(obj, method_name, args, vars)
            }
            _ => Err("only direct function calls and method calls supported in codegen".into()),
        }
    }

    /// Call a variable that holds a first-class function pointer.
    fn compile_fn_ptr_var_call(
        &mut self,
        name: &str,
        alloca: inkwell::values::PointerValue<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // 0.35.14 (DX backlog #18): tuple-extracted fn bindings (`let f =
        // t.0`) hold the pointer as a ptrtoint i64 slot — inttoptr it back.
        // Direct `let f = func_name` bindings hold a pointer slot.
        let loaded = self.build_load(ty, alloca, &format!("{}_fn", name))?;
        let fn_ptr = match loaded {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::IntValue(iv) => self
                .builder
                .build_int_to_ptr(
                    iv,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "fn_ptr_from_i64",
                )
                .map_err(|e| CompileError::LlvmError(format!("fn ptr inttoptr: {e}")))?,
            other => {
                return Err(CompileError::Generic(format!(
                    "fn-pointer variable '{}' holds an unexpected value {:?}",
                    name,
                    other.get_type()
                )))
            }
        };
        let compiled_args = self.compile_arg_values(args, vars)?;
        let i64_ty = self.context.i64_type();
        let all_meta: Vec<_> = compiled_args
            .iter()
            .map(|arg| basic_value_to_metadata_type(arg))
            .collect::<Result<Vec<_>, _>>()?;
        // 2026-08-06 (§7-#81): the return type was hard-coded to i64, so a
        // first-class function pointer returning f64 (or a struct) called
        // with an i64-returning indirect signature — the callee wrote its
        // f64 result into %xmm0 while the caller read a garbage i64 from
        // %rax (e.g. 4618722892845154304 instead of 6.25). Recover the
        // declared return type from var_types (mirrors closure calls).
        let ret_type = self
            .var_types
            .get(name)
            .and_then(|ty| self.closure_return_llvm_type(ty))
            .unwrap_or(BasicTypeEnum::IntType(i64_ty));
        let indirect_fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&all_meta, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&all_meta, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&all_meta, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&all_meta, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&all_meta, false),
            _ => i64_ty.fn_type(&all_meta, false),
        };
        let fn_ptr_typed = self.build_pointer_cast(
            fn_ptr,
            self.context.ptr_type(inkwell::AddressSpace::default()),
            "fn_typed",
        )?;
        let call_args: Vec<_> = compiled_args
            .iter()
            .map(|arg| types::basic_value_to_metadata_value(arg, i64_ty))
            .collect();
        let call = self
            .builder
            .build_indirect_call(indirect_fn_type, fn_ptr_typed, &call_args, "fn_ptr_call")
            .map_err(|e| CompileError::LlvmError(format!("fn ptr call error: {}", e)))?;
        Ok(call_try_basic_value(&call).unwrap_or(i64_ty.const_int(0, false).into()))
    }

    /// Call a user-defined enum variant constructor.
    fn compile_custom_enum_constructor_call(
        &mut self,
        type_name: &str,
        method_name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ctor_name = format!("{}_{}", type_name, method_name);
        let function = self
            .module
            .get_function(&ctor_name)
            .ok_or_else(|| format!("enum constructor '{}' not registered", ctor_name))?;
        let compiled_args = self.compile_arg_values(args, vars)?;
        let call_args = self.maybe_pack_enum_ctor_args(&compiled_args, function)?;
        self.emit_direct_call(function, &call_args, "enum_ctor")
    }

    /// Coerce a compiled arg to the LLVM type expected by an enum-ctor payload
    /// field (or the sole packed param).
    ///
    /// - string field `{ptr,i64}` + raw `i8*` (string literal) → wrap via strlen
    /// - other struct field + alloca pointer → load by value
    /// - integer width mismatch → trunc/sext
    pub(in crate::codegen) fn coerce_value_to_expected_type(
        &self,
        arg: BasicValueEnum<'ctx>,
        expected: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (arg, expected) {
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => {
                let fields = st.get_field_types();
                // Mimi string is { i8*, i64 }; List is { i64, i8* } — order differs.
                let is_string = fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(
                        fields[1],
                        BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                    );
                if is_string {
                    self.wrap_raw_string_ptr(pv)
                } else {
                    self.build_load(BasicTypeEnum::StructType(st), pv, "coerce_struct_load")
                }
            }
            (BasicValueEnum::IntValue(arg_iv), BasicTypeEnum::IntType(exp_it)) => {
                let arg_bw = arg_iv.get_type().get_bit_width();
                let exp_bw = exp_it.get_bit_width();
                if arg_bw == exp_bw {
                    Ok(arg)
                } else if arg_bw > exp_bw {
                    Ok(self
                        .builder
                        .build_int_truncate(arg_iv, exp_it, "coerce_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("arg trunc: {}", e)))?
                        .into())
                } else {
                    Ok(self
                        .builder
                        .build_int_s_extend(arg_iv, exp_it, "coerce_sext")
                        .map_err(|e| CompileError::LlvmError(format!("arg s_ext: {}", e)))?
                        .into())
                }
            }
            _ => Ok(arg),
        }
    }

    /// If an enum constructor expects a single packed struct (multi-field variant
    /// or single struct payload like `string` / `List<T>`), coerce each arg to the
    /// expected field type and pack. Single non-struct args pass through after coerce.
    pub(in crate::codegen) fn maybe_pack_enum_ctor_args(
        &mut self,
        compiled_args: &[BasicValueEnum<'ctx>],
        function: inkwell::values::FunctionValue<'ctx>,
    ) -> Result<Vec<BasicValueEnum<'ctx>>, CompileError> {
        if function.count_params() != 1 {
            return Ok(compiled_args.to_vec());
        }
        let param = function
            .get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("expected at least one param".into()))?;
        let BasicValueEnum::StructValue(param_sv) = param else {
            // Primitive single payload (i32/f64/…): coerce width only.
            if compiled_args.len() == 1 {
                let expected = param.get_type();
                return Ok(vec![
                    self.coerce_value_to_expected_type(compiled_args[0], expected)?
                ]);
            }
            return Ok(compiled_args.to_vec());
        };
        let struct_ty = param_sv.get_type();
        let field_tys = struct_ty.get_field_types();

        if compiled_args.len() > 1 {
            // Multi-arg variant → one packed struct param.
            if field_tys.len() != compiled_args.len() {
                return Err(CompileError::LlvmError(format!(
                    "enum ctor pack: {} args for {}-field payload",
                    compiled_args.len(),
                    field_tys.len()
                )));
            }
            let mut struct_val = struct_ty.get_undef();
            for (i, arg) in compiled_args.iter().enumerate() {
                let coerced = self.coerce_value_to_expected_type(*arg, field_tys[i])?;
                let agg = self
                    .builder
                    .build_insert_value(struct_val, coerced, i as u32, "packed_field")
                    .map_err(|e| {
                        CompileError::LlvmError(format!("pack enum ctor arg {}: {}", i, e))
                    })?;
                struct_val = agg.into_struct_value();
            }
            return Ok(vec![BasicValueEnum::StructValue(struct_val)]);
        }

        // Single arg for struct payload (string, List, nested enum, …).
        if compiled_args.len() == 1 {
            let coerced = self.coerce_value_to_expected_type(
                compiled_args[0],
                BasicTypeEnum::StructType(struct_ty),
            )?;
            return Ok(vec![coerced]);
        }
        Ok(compiled_args.to_vec())
    }

    /// Extract the LLVM return type of a closure-typed variable so that indirect
    /// calls use the correct ABI (especially for tuple/struct/float returns).
    fn closure_return_llvm_type(&self, ty: &Type) -> Option<BasicTypeEnum<'ctx>> {
        match ty.unlocated() {
            Type::Func(_, ret) | Type::ExternFunc(_, ret) => self.llvm_type_for(ret),
            Type::Ref(_, inner) | Type::RefMut(_, inner) => self.closure_return_llvm_type(inner),
            _ => None,
        }
    }

    pub(in crate::codegen) fn compile_call(
        &mut self,
        name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ordered = self.reorder_named_args(name, args)?;
        if let Some(arity_map) = self.resolved_function_arity.as_ref() {
            if let Some(arity) = arity_map.get(name) {
                let has_defaults = self
                    .func_defs
                    .get(name)
                    .is_some_and(|f| f.params.iter().any(|p| p.default_value.is_some()));
                if !has_defaults && ordered.len() != *arity {
                    return Err(CompileError::Generic(format!(
                        "function '{}' expects {} arguments, got {} (checked directory)",
                        name,
                        arity,
                        ordered.len()
                    )));
                }
            }
        }
        let mut compiled_args = self.compile_arg_values(&ordered, vars)?;
        // Use ordered exprs for list-mutation/borrow paths below.
        let args = ordered.as_slice();

        // v0.28.29 fix for mimichat gap #2: list-mutating builtins (`push`,
        // `pop`) take a `*List` pointer at the LLVM level. When the caller
        // passes a local `let l: List<T> = ...`, the alloca for `l` is the
        // authoritative location. Naively `compile_arg_values` would `load`
        // the struct out of the alloca, then `compile_push` would mutate a
        // freshly-allocated temporary — discarding the changes and leaving
        // `l.data` pointing at the (already-freed) pre-mutation buffer,
        // causing double free / SIGSEGV on the next push.
        //
        // For mutating list builtins whose var slot is a `{i64, ptr}` struct
        // (i.e. `let l: List<T> = from_json::<List<T>>(...)` where the codegen
        // store the list value-by-value), swap args[0] from the loaded
        // StructValue back to the original alloca pointer so the mutation
        // is visible. Skip the swap when the var is already a list pointer
        // (e.g. list literals — the loaded value is already a *List that
        // `require_list_pointer` returns as-is, which is the correct LLVM
        // pointer for gep against the list struct).
        if matches!(name, "push" | "pop") && !args.is_empty() {
            // v0.31.6: unlocate args[0] — the v0.31.1 Span/Origin pass wraps
            // call arguments in Expr::Located, so matching `&args[0]` against
            // `Expr::Ident`/`Expr::Field` silently missed and the alloca-swap
            // below never fired, letting push/pop mutate a discarded temporary
            // (double free / stale buffer). Match the unwrapped node instead.
            match args[0].unlocated() {
                Expr::Ident(var_name) => {
                    if self.is_list_type_name(&self.infer_object_type(&args[0], vars)) {
                        if let Some(&(alloca, var_ty)) = vars.get(var_name) {
                            if matches!(var_ty, BasicTypeEnum::StructType(_)) {
                                compiled_args[0] = BasicValueEnum::PointerValue(alloca);
                            }
                        }
                    }
                }
                // Handle self.field = push(self.field, val) — get GEP pointer to field slot
                Expr::Field(obj_expr, field_name) => {
                    if let Expr::Ident(obj_name) = obj_expr.unlocated() {
                        if obj_name == "self" {
                            if let Ok(field_gep) =
                                self.compile_field_gep(obj_expr, field_name, vars)
                            {
                                if self.is_list_type_name(&self.infer_object_type(&args[0], vars)) {
                                    compiled_args[0] = BasicValueEnum::PointerValue(field_gep);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        self.maybe_convert_callback_args(name, &mut compiled_args)?;
        self.maybe_load_reprc_struct_args_for_extern(name, &mut compiled_args)?;
        self.coerce_args_to_param_types(name, &mut compiled_args)?;

        // read_lines_each needs the closure struct (not metadata-only) to
        // build a void(char*) C thunk that re-wraps lines as Mimi strings.
        if name == "read_lines_each" {
            return self.compile_read_lines_each_call(&compiled_args);
        }

        let mut metadata_args: Vec<_> = compiled_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();

        if (name == "len" || name == "is_empty") && args.len() == 1 {
            self.pending_len_is_string = self.expr_is_string(&args[0]);
            // is_empty: map and set both lower to bare i64 handles — the
            // inferred type name disambiguates them for compile_is_empty.
            self.pending_is_empty_kind = if name == "is_empty" {
                classify_is_empty_kind(&self.infer_object_type(&args[0], vars))
            } else {
                None
            };
        }
        if name == "to_string" && args.len() == 1 {
            let arg_type = self.infer_object_type(&args[0], vars);
            self.pending_to_string_is_any = arg_type == "Any" || arg_type == "any";
            self.pending_to_string_arg_type = Some(arg_type);
        }
        if (name == "to_int" || name == "to_float") && args.len() == 1 {
            let arg_type = self.infer_object_type(&args[0], vars);
            self.pending_to_number_is_any = arg_type == "Any" || arg_type == "any";
        }
        if (name == "str_parse_int" || name == "str_parse_float") && args.len() == 1 {
            let arg_type = self.infer_object_type(&args[0], vars);
            self.pending_to_number_is_any = arg_type == "Any" || arg_type == "any";
        }
        if name == "push" && args.len() == 2 {
            let list_type = self.infer_object_type(&args[0], vars);
            if let Some(elem_type) = Self::strip_list_element_type(&list_type) {
                self.pending_push_elem_type = Some(elem_type);
            }
        }
        // Audit wave2 (D-5a): sum(List<f64>) — hand the element type to the
        // builtin so f64 bit patterns are not accumulated as i64.
        if name == "sum" && args.len() == 1 {
            let list_type = self.infer_object_type(&args[0], vars);
            self.pending_sum_elem_type = Self::strip_list_element_type(&list_type);
        }
        // Audit P0-06: pop(List<f64>/string/record) must decode the type-erased
        // slot instead of returning the raw i64/bit pattern.
        if name == "pop" && args.len() == 1 {
            let list_type = self.infer_object_type(&args[0], vars);
            self.pending_pop_elem_type = Self::strip_list_element_type(&list_type);
        }
        // 0.35.20 (#6): zip/enumerate — hand the product-tuple element type to
        // the builtin so pairs are heap-packed with the formatter's layout
        // (string fields inline {ptr,len}); raw i64-slot pairs misdisplay.
        if name == "zip" && args.len() == 2 {
            let a = self.infer_object_type(&args[0], vars);
            let b = self.infer_object_type(&args[1], vars);
            if let (Some(ea), Some(eb)) = (
                Self::strip_list_element_type(&a),
                Self::strip_list_element_type(&b),
            ) {
                self.pending_zip_pair_type = Some(format!("({}, {})", ea, eb));
            }
        }
        if name == "enumerate" && args.len() == 1 {
            let src = self.infer_object_type(&args[0], vars);
            if let Some(elem) = Self::strip_list_element_type(&src) {
                self.pending_zip_pair_type = Some(format!("(i32, {})", elem));
            }
        }
        let builtin_available = crate::codegen::builtins::is_builtin(name);
        let user_func_matches = self.user_func_signature_matches(name, args);
        if builtin_available && !user_func_matches {
            // 2026-08-06 (audit 1g): str_contains is polymorphic in the VM —
            // (string|List|Set, value). A List haystack used to be rejected by
            // the guard (registered VM-only gap); route it to compile_contains
            // (element comparison) for VM parity. (audit 1k) Set haystacks
            // (bare i64 handle) route to mimi_set_contains — same VM parity.
            if name == "str_contains" && !args.is_empty() {
                let hay_ty = self.infer_object_type(&args[0], vars);
                if hay_ty.starts_with("Set") {
                    if compiled_args.len() < 2 {
                        return Err(CompileError::WrongArgCount(
                            "str_contains expects 2 arguments".into(),
                        ));
                    }
                    let i64_ty = self.context.i64_type();
                    return self
                        .compile_set_contains_fn(
                            types::basic_value_to_metadata_value(&compiled_args[0], i64_ty),
                            types::basic_value_to_metadata_value(&compiled_args[1], i64_ty),
                        )
                        .map_err(|e| CompileError::Generic(e.to_string()));
                }
                if hay_ty.starts_with("List") {
                    return self
                        .compile_contains(&metadata_args)
                        .map_err(|e| CompileError::Generic(e.to_string()));
                }
            }
            // 2026-08-06 (audit 1f): exec_safe(prog, arg1, arg2, …) is
            // varargs — every argument after the program must be a string.
            // codegen packed them into argv without checking, so a List
            // vararg became garbage argv (silent, red-line #2); the VM fails
            // loud with E0800 "all arguments must be strings".
            if name == "exec_safe" && args.len() > 1 {
                for (i, arg) in args.iter().enumerate().skip(1) {
                    let arg_ty = self.infer_object_type(arg, vars);
                    if self.is_definitely_not_string(&arg_ty) {
                        return Err(CompileError::TypeMismatch(format!(
                            "exec_safe: all arguments must be strings (argument {} is {})",
                            i, arg_ty
                        )));
                    }
                }
            }
            // 2026-08-06 (audit 1c): `contains` is polymorphic in the VM
            // ((string|List|Set, value)); compile_contains only handles List,
            // and a string haystack arrives as a raw pointer so load_list_len
            // would read a string struct and SIGSEGV. Redirect string
            // haystacks to str_contains (strstr) and guard the needle.
            // (audit 1j) Set haystacks are bare i64 handles — route to
            // mimi_set_contains (was a VM-only gap).
            if name == "contains" && !args.is_empty() {
                let hay_ty = self.infer_object_type(&args[0], vars);
                if hay_ty.starts_with("Set") {
                    if compiled_args.len() < 2 {
                        return Err(CompileError::WrongArgCount(
                            "contains expects 2 arguments".into(),
                        ));
                    }
                    let i64_ty = self.context.i64_type();
                    return self
                        .compile_set_contains_fn(
                            types::basic_value_to_metadata_value(&compiled_args[0], i64_ty),
                            types::basic_value_to_metadata_value(&compiled_args[1], i64_ty),
                        )
                        .map_err(|e| CompileError::Generic(e.to_string()));
                }
                if hay_ty == "string" {
                    if args.len() >= 2 {
                        let needle_ty = self.infer_object_type(&args[1], vars);
                        if self.is_definitely_not_string(&needle_ty) {
                            return Err(CompileError::TypeMismatch(format!(
                                "contains expects a string needle for a string haystack, found {}",
                                needle_ty
                            )));
                        }
                    }
                    return self
                        .compile_str_contains(&metadata_args)
                        .map_err(|e| CompileError::Generic(e.to_string()));
                }
            }
            // 2026-08-06 (audit 1): string-only builtins — reject a definitely
            // non-string argument at compile time. The LLVM value of a
            // List arrives as a raw pointer (List value = ptr to {i64, ptr}),
            // indistinguishable from a string pointer in the emitter, so
            // `str_trim([1,2,3])` used to strlen a list struct → garbage /
            // panic. Fail loud (VM parity: E0800 at runtime). Guards every
            // string argument position of the whole str_* / regex_* family.
            if let Some(pos) = Self::string_only_builtin_string_args(name) {
                for &p in pos {
                    let p = p as usize;
                    if p >= args.len() {
                        break; // arg-count error is reported by the callee
                    }
                    let arg_ty = self.infer_object_type(&args[p], vars);
                    if self.is_definitely_not_string(&arg_ty) {
                        return Err(CompileError::TypeMismatch(format!(
                            "{} expects a string argument at position {}, found {}",
                            name, p, arg_ty
                        )));
                    }
                }
            }
            // to_int/to_float aggregate guard: a statically known List/Map/
            // record argument cannot be converted — the VM fails loud with
            // "cannot convert this type" (E0800), while the native runtime
            // parser would strlen the aggregate pointer and report a
            // misleading "invalid digit" parse error. Reject at compile time
            // with the VM-aligned message.
            if Self::is_conversion_builtin(name) && !args.is_empty() {
                let arg_ty = self.infer_object_type(&args[0], vars);
                if self.is_definitely_not_convertible(&arg_ty) {
                    return Err(CompileError::TypeMismatch(format!(
                        "[E0800] {} cannot convert this type ({})",
                        name, arg_ty
                    )));
                }
            }
            // 0.39.136 architecture: typed to_json routing moved into the
            // shared emit_typed_to_json_dispatch (also used by the resolved
            // emitter). obj_type comes from infer_object_type here and from
            // resolved_type_display_name there.
            if name == "to_json" && !metadata_args.is_empty() {
                let obj_type = self.infer_object_type(&args[0], vars);
                // Use the *actual* LLVM storage type of the argument when it is
                // a bare variable: `vars` already records the true box type the
                // legacy emitter produced (force-heap `{i1,i64}` for containers).
                // For by-value / non-variable arguments (`to_json(Some((1, 2)))`,
                // `to_json(f(x))`, …) the legacy emitter still force-heaps
                // container payloads, so fall back to the force-heap `llvm_type_for`
                // layout — otherwise `actual_ty` is `None` and the recursive
                // serializer assumes the embedded layout, reading a boxed pointer
                // as an inline struct and emitting garbage.
                let actual_ty = match args[0].unlocated() {
                    crate::ast::Expr::Ident(name) => {
                        vars.get(name.as_str()).map(|(_, ty)| *ty)
                    }
                    _ => None,
                };
                let actual_ty = actual_ty.or_else(|| {
                    crate::codegen::expr::call::helpers::parse_type_str(&obj_type)
                        .and_then(|t| self.llvm_type_for(&t))
                });
                if let Some(value) = self.emit_typed_to_json_dispatch(
                    &obj_type,
                    metadata_args[0],
                    actual_ty,
                )? {
                    return Ok(value);
                }
            }
            // P0-3: for the print/println/eprintln family only, convert
            // boolean args to "true"/"false" string pointers before
            // handing them to the builtin dispatch. Other builtins
            // (e.g. atomic_bool_new) legitimately expect an i64, so the
            // conversion must stay scoped to print sinks.
            if matches!(name, "println" | "print" | "eprintln" | "format") {
                self.pending_print_arg_types = args
                    .iter()
                    .map(|a| self.infer_object_type(a, vars))
                    .collect();
                for (i, src) in args.iter().enumerate() {
                    if i >= metadata_args.len() {
                        break;
                    }
                    if let Some(replaced) = self.maybe_bool_to_string(
                        src,
                        match metadata_args[i] {
                            BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                            BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                            BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
                            BasicMetadataValueEnum::StructValue(sv) => sv.into(),
                            _ => continue,
                        },
                    ) {
                        metadata_args[i] = match replaced {
                            BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(iv),
                            BasicValueEnum::FloatValue(fv) => {
                                BasicMetadataValueEnum::FloatValue(fv)
                            }
                            BasicValueEnum::PointerValue(pv) => {
                                BasicMetadataValueEnum::PointerValue(pv)
                            }
                            BasicValueEnum::StructValue(sv) => {
                                BasicMetadataValueEnum::StructValue(sv)
                            }
                            _ => continue,
                        };
                    }
                }
            }
            return self
                .compile_builtin_call(name, &metadata_args)
                .map_err(|e| CompileError::Generic(e.to_string()));
        }

        if let Some((type_name, _ordinal)) = self.find_variant_owner(name) {
            let ctor_name = format!("{}_{}", type_name, name);
            if let Some(function) = self.module.get_function(&ctor_name) {
                let call_args = self.maybe_pack_enum_ctor_args(&compiled_args, function)?;
                // emit_direct_call: int width adjust + load list/record allocas
                // when the ctor takes a by-value struct payload.
                let result = self.emit_direct_call(function, &call_args, "enum_ctor")?;
                // L6: a Packed variant payload is a heap box (malloc'd by the
                // ctor, ptrtoint-encoded into the i64 slot). Register it
                // (HeapEntry::Ptr) so the scope-exit free releases it when the
                // value is consumed locally. If the value is RETURNED, the
                // return-claim (block.rs Stmt::Return) adds the box pointer to
                // claimed_returned_envs so this registration is skipped, and the
                // caller re-registers it via HeapEntry::EnumBox (tag-conditional
                // free). Single/None variants are inline (not boxed) → skipped.
                if self.variant_payload_is_boxed(name) {
                    if let BasicValueEnum::StructValue(sv) = result {
                        if sv.get_type().count_fields() == 2 {
                            let payload = self
                                .builder
                                .build_extract_value(sv, 1, "enum_ctor_box_i64")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("enum ctor box extract: {e}"))
                                })?;
                            if let BasicValueEnum::IntValue(iv) = payload {
                                let box_ptr = self.build_int_to_ptr(
                                    iv,
                                    self.context.ptr_type(inkwell::AddressSpace::default()),
                                    "enum_ctor_box",
                                )?;
                                // register_heap_box null-inits the slot at the
                                // entry block so a ctor in an untaken conditional
                                // branch frees null, not garbage (see mod.rs).
                                self.register_heap_box(box_ptr);
                            }
                        }
                    }
                }
                return Ok(result);
            }
            return Err(format!("enum constructor '{}' not registered", ctor_name).into());
        }

        match name {
            "Ok" | "Some" | "Err" | "None" => {
                // Wrap raw string literal pointers into string structs before
                // passing to constructors (they use coerce_to_i64_slot which
                // needs the full {ptr, i64} struct, not a raw i8*).
                // Ok/Some/Err/None are not in func_defs, so maybe_wrap_string_args_for_call
                // won't find them — wrap manually based on arg expr type.
                for (i, arg_expr) in args.iter().enumerate() {
                    if i >= compiled_args.len() {
                        break;
                    }
                    if let BasicValueEnum::PointerValue(pv) = compiled_args[i] {
                        // Check if the arg is a string literal or string-producing expr
                        if matches!(arg_expr.unlocated(), Expr::Literal(Lit::String(_))) {
                            compiled_args[i] = self.wrap_raw_string_ptr(pv)?;
                        }
                    }
                }
                return self.compile_constructor(name, compiled_args);
            }
            _ => {}
        }

        self.maybe_wrap_string_args_for_call(name, args, &mut compiled_args)?;
        self.maybe_convert_list_args_to_values(name, &mut compiled_args)?;
        self.maybe_convert_record_args_to_values(name, &mut compiled_args)?;
        self.maybe_wrap_named_fn_args_to_closures(name, args, &mut compiled_args)?;
        // Run after value-shape conversions: borrowed parameters must be the
        // final authority and pass storage addresses, never copied structs.
        self.prepare_borrowed_user_args(name, args, vars, &mut compiled_args)?;

        metadata_args = compiled_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();

        if self.extern_func_defs.contains_key(name) {
            self.generate_extern_fn(name)?;
        }
        self.emit_named_call(name, args, &mut compiled_args, &metadata_args, vars)
    }

    /// G1b: Convert closure struct args to thunk pointers for extern callback params.
    fn maybe_convert_callback_args(
        &mut self,
        name: &str,
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(param_types) = self.extern_param_types.get(name).cloned() else {
            return Ok(());
        };
        for (i, compiled) in compiled_args.iter_mut().enumerate() {
            if i >= param_types.len() {
                break;
            }
            let (cb_params, cb_ret) = match param_types[i].unlocated() {
                crate::ast::Type::ExternFunc(p, r) => (p.as_slice(), r.as_ref()),
                crate::ast::Type::Func(p, r) => (p.as_slice(), r.as_ref()),
                _ => continue,
            };
            if let BasicValueEnum::StructValue(sv) = compiled {
                let struct_ty = sv.get_type();
                if struct_ty.get_field_types().len() == 2 {
                    let fn_ptr = self
                        .build_extract_value((*sv).into(), 0, "cb_fn_ptr")?
                        .into_pointer_value();
                    let env_ptr = self
                        .build_extract_value((*sv).into(), 1, "cb_env_ptr")?
                        .into_pointer_value();
                    let thunk_entry = self
                        .get_or_create_callback_thunk(cb_params, cb_ret)
                        .map_err(|e| CompileError::LlvmError(format!("callback thunk: {}", e)))?;
                    self.build_store(thunk_entry.fn_ptr_global.as_pointer_value(), fn_ptr)?;
                    self.build_store(thunk_entry.env_ptr_global.as_pointer_value(), env_ptr)?;
                    self.pending_callback_tls
                        .push(thunk_entry.fn_ptr_global.as_pointer_value());
                    self.pending_callback_tls
                        .push(thunk_entry.env_ptr_global.as_pointer_value());
                    let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let thunk_ptr = thunk_entry.thunk_fn.as_global_value().as_pointer_value();
                    let casted = self.build_pointer_cast(thunk_ptr, i8_ptr_ty, "thunk_i8")?;
                    *compiled = casted.into();
                }
            }
        }
        Ok(())
    }

    /// For extern functions: load struct values from pointers for repr(C) struct-by-value params.
    fn maybe_load_reprc_struct_args_for_extern(
        &self,
        name: &str,
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(ef) = self.extern_func_defs.get(name) else {
            return Ok(());
        };
        for (i, arg) in compiled_args.iter_mut().enumerate() {
            if i >= ef.params.len() {
                break;
            }
            if let crate::ast::Type::Name(n, _) = ef.params[i].ty.unlocated() {
                if self.repr_c_record_names.contains(n.as_str()) {
                    if let BasicValueEnum::PointerValue(pv) = arg {
                        if let Some(&BasicTypeEnum::StructType(sty)) =
                            self.type_llvm.get(n.as_str())
                        {
                            let loaded = self.build_load(
                                BasicTypeEnum::StructType(sty),
                                *pv,
                                &format!("{}_extern_val", n),
                            )?;
                            *arg = loaded;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Convert pointer-valued list arguments to struct values when the function
    /// parameter expects List<T> (passed by value).
    fn maybe_convert_list_args_to_values(
        &self,
        name: &str,
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(fdef) = self.func_defs.get(name) else {
            return Ok(());
        };
        for (i, arg) in compiled_args.iter_mut().enumerate() {
            if i < fdef.params.len() {
                if let Type::Name(tn, _) = fdef.params[i].ty.unlocated() {
                    if tn == "List" {
                        if let Some(param_llvm) = self.llvm_type_for(&fdef.params[i].ty) {
                            if let BasicValueEnum::PointerValue(pv) = arg {
                                let loaded = self.build_load(
                                    param_llvm,
                                    *pv,
                                    &format!("{}_struct_arg", &fdef.params[i].name),
                                )?;
                                *arg = loaded;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Convert pointer-valued record arguments to struct values when the function
    /// parameter expects a record type (passed by value in LLVM).
    fn maybe_convert_record_args_to_values(
        &self,
        name: &str,
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(fdef) = self.func_defs.get(name) else {
            return Ok(());
        };
        for (i, arg) in compiled_args.iter_mut().enumerate() {
            if i < fdef.params.len() {
                // v0.31.25: skip view/mutate params — they use the reference ABI
                // (pointer to caller's storage). Loading the struct value here
                // would destroy the pointer before prepare_borrowed_user_args runs.
                if fdef.params[i].borrow.is_some() {
                    continue;
                }
                if let Type::Name(tn, _) = fdef.params[i].ty.unlocated() {
                    if tn != "List" && self.type_defs.contains_key(tn) {
                        if let BasicValueEnum::PointerValue(pv) = arg {
                            if let Some(param_llvm) = self.llvm_type_for(&fdef.params[i].ty) {
                                let loaded = self.build_load(
                                    param_llvm,
                                    *pv,
                                    &format!("{}_struct_arg", &fdef.params[i].name),
                                )?;
                                *arg = loaded;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Convert function pointers to closure structs when the parameter type expects func(T) -> U.
    fn maybe_wrap_named_fn_args_to_closures(
        &mut self,
        name: &str,
        args: &[Expr],
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(fdef) = self.func_defs.get(name) else {
            return Ok(());
        };
        let wrappers: Vec<Option<String>> = args
            .iter()
            .enumerate()
            .map(|(i, arg_expr)| {
                if i < fdef.params.len()
                    && matches!(fdef.params[i].ty.unlocated(), Type::Func(_, _))
                {
                    if let Expr::Ident(fn_name) = arg_expr.unlocated() {
                        return Some(fn_name.clone());
                    }
                }
                None
            })
            .collect();

        for (i, fn_name_opt) in wrappers.into_iter().enumerate() {
            if let Some(fn_name) = fn_name_opt {
                if let BasicValueEnum::PointerValue(_pv) = compiled_args[i] {
                    let wrapper = self.get_or_create_closure_wrapper(&fn_name)?;
                    let closure_ty = crate::codegen::types::closure_struct_type(self.context);
                    let closure_alloca =
                        self.build_alloca(BasicTypeEnum::StructType(closure_ty), "closure_arg")?;
                    let fn_gep = self
                        .gep()
                        .build_struct_gep(closure_ty, closure_alloca, 0, "fn_gep")
                        .map_err(|e| CompileError::LlvmError(format!("fn gep: {}", e)))?;
                    self.build_store(fn_gep, BasicValueEnum::PointerValue(wrapper))?;
                    let env_gep = self
                        .gep()
                        .build_struct_gep(closure_ty, closure_alloca, 1, "env_gep")
                        .map_err(|e| CompileError::LlvmError(format!("env gep: {}", e)))?;
                    let null_i8 = self
                        .context
                        .ptr_type(inkwell::AddressSpace::default())
                        .const_null();
                    self.build_store(env_gep, BasicValueEnum::PointerValue(null_i8))?;
                    let loaded = self.build_load(
                        BasicTypeEnum::StructType(closure_ty),
                        closure_alloca,
                        "closure_loaded",
                    )?;
                    compiled_args[i] = loaded;
                }
            }
        }
        Ok(())
    }

    /// Emit a call to a function looked up by name, with generic monomorphization fallback.
    fn emit_named_call(
        &mut self,
        name: &str,
        args: &[Expr],
        compiled_args: &mut [BasicValueEnum<'ctx>],
        metadata_args: &[BasicMetadataValueEnum<'ctx>],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // For non-generic functions, use the symbol as-is if it already exists.
        // Generic functions must go through compile_generic_func for monomorphization.
        let is_generic = self
            .func_defs
            .get(name)
            .is_some_and(|f| !f.generics.is_empty());
        if !is_generic {
            // V-11 (audit 2026-08-05): an active nested-function shadow
            // redirects the bare name to the mangled symbol. Must precede
            // the plain module lookup — the flat LLVM symbol namespace would
            // otherwise resolve the call to the shadowed GLOBAL function
            // (type-mismatched arguments → invalid IR).
            if let Some((symbol, _)) = self.nested_shadow_symbols.get(name) {
                if let Some(function) = self.module.get_function(symbol) {
                    return self.emit_function_call(function, name, metadata_args);
                }
            }
            // Extern wrappers are keyed by declaration name in the wrapper map
            // (wrapper itself is named `{name}.extern_wrapper` since 0.34.35b).
            // Check the wrapper map first to call the correct function.
            if let Some(&wrapper) = self.extern_wrapper_fns.get(name) {
                return self.emit_function_call(wrapper, name, metadata_args);
            }
            // 0.34.35b (M-001) guard: extern 符号名 = 声明名，若 wrapper 缺失
            // （不应发生），module.get_function(name) 会命中 extern 原函数、
            // 跳过 wrapper 的 ABI 参数转换——必须显式拒绝而非静默误编译。
            if self.extern_func_defs.contains_key(name) {
                return Err(CompileError::LlvmError(format!(
                    "extern '{}': wrapper not emitted (declared but not compiled)",
                    name
                )));
            }
            if let Some(function) = self.module.get_function(name) {
                return self.emit_function_call(function, name, metadata_args);
            }
        }

        let (mangled, callee_map) = if let Some(fdef) = self.func_defs.get(name) {
            if !fdef.generics.is_empty() {
                let mut callee_map: HashMap<String, Type> = HashMap::new();
                let generic_names: Vec<String> =
                    fdef.generics.iter().map(|gp| gp.name.clone()).collect();
                for (i, param) in fdef.params.iter().enumerate() {
                    if i >= args.len() {
                        break;
                    }
                    if let Some(arg_type) = self.expr_type_of(&args[i], vars) {
                        infer_generic_args(&param.ty, &arg_type, &generic_names, &mut callee_map);
                    }
                }
                // Fallback for simple direct generic params (e.g. `identity<T>(x: T)`)
                // when expr_type_of couldn't produce a type.
                for gp in &fdef.generics {
                    if !callee_map.contains_key(&gp.name) {
                        for (i, param) in fdef.params.iter().enumerate() {
                            if i >= args.len() {
                                break;
                            }
                            if Self::type_references_generic(&param.ty, &gp.name) {
                                if let Some(arg_type) = self.expr_type_of(&args[i], vars) {
                                    callee_map.insert(gp.name.clone(), arg_type);
                                    break;
                                }
                            }
                        }
                    }
                }
                let mangled = Self::mangle_name(name, &callee_map);
                (mangled, callee_map)
            } else {
                (
                    Self::mangle_name(name, &self.type_map),
                    self.type_map.clone(),
                )
            }
        } else {
            (
                Self::mangle_name(name, &self.type_map),
                self.type_map.clone(),
            )
        };

        // Compile the specialized generic function on demand if it doesn't exist yet.
        if !callee_map.is_empty() {
            self.type_map = callee_map.clone();
        }
        // GENERIC-SHADOW-MONO-001: the mangled name may already exist as a
        // bare forward DECLARATION planted by another emitter's signature
        // pre-install — `is_none()` then skipped instantiation nondeterministically
        // (HashMap-ordered emission) and linked calls against an empty
        // declaration. Compile whenever no DEFINITION exists yet.
        if self
            .module
            .get_function(&mangled)
            .map(|f| f.count_basic_blocks() == 0)
            .unwrap_or(true)
        {
            if let Some(fdef) = self.func_defs.get(name).cloned() {
                if !fdef.generics.is_empty() {
                    self.compile_generic_func(&fdef, &callee_map).map_err(|e| {
                        CompileError::Generic(format!(
                            "failed to monomorphize function '{}': {}",
                            name, e
                        ))
                    })?;
                }
            }
        }

        if let Some(function) = self.module.get_function(&mangled) {
            // 0.39.135 (L1 parity): monomorphized instances take user records
            // and newtypes BY VALUE, while record expressions compile to
            // alloca pointers. Passing the bare pointer where the callee
            // expects {fields} made LLVM reinterpret address bits as field
            // data — `let q = pass(p); println(q.v)` printed garbage (and
            // could segfault). Load the struct through the pointer when the
            // substituted param is a named user aggregate. Strings are
            // excluded: their raw-C-pointer arg form is wrapped later by
            // coerce_args_to_function.
            if !callee_map.is_empty() {
                if let Some(generic_def) = self.func_defs.get(name).cloned() {
                    for (i, gp) in generic_def.params.iter().enumerate() {
                        if i >= compiled_args.len() {
                            break;
                        }
                        let substituted = self.resolve_type(&gp.ty);
                        let is_named_aggregate = match substituted.unlocated() {
                            Type::Name(n, args_) if args_.is_empty() => !matches!(
                                n.as_str(),
                                "string"
                                    | "i8"
                                    | "i16"
                                    | "i32"
                                    | "i64"
                                    | "u8"
                                    | "u16"
                                    | "u32"
                                    | "u64"
                                    | "usize"
                                    | "isize"
                                    | "f32"
                                    | "f64"
                                    | "bool"
                                    | "char"
                            ),
                            _ => false,
                        };
                        if is_named_aggregate
                            && matches!(compiled_args[i], BasicValueEnum::PointerValue(_))
                        {
                            if let Some(pt) = function.get_nth_param(i as u32) {
                                if let BasicTypeEnum::StructType(_) = pt.get_type() {
                                    let pv = match compiled_args[i] {
                                        BasicValueEnum::PointerValue(pv) => pv,
                                        _ => continue,
                                    };
                                    compiled_args[i] =
                                        self.build_load(pt.get_type(), pv, "mono_byval_arg")?;
                                }
                            }
                        }
                    }
                }
            }
            // Generic functions were skipped during pre-call coercion; the
            // monomorphized function has concrete parameter types, so coerce
            // the already-compiled args against them now (int width, int↔float).
            self.coerce_args_to_function(function, compiled_args)?;
            let metadata_args: Vec<_> = compiled_args
                .iter()
                .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
                .collect();
            let call = self.build_call(function, &metadata_args, "call")?;
            Ok(call_try_basic_value(&call)
                .unwrap_or(self.context.i64_type().const_int(0, false).into()))
        } else if let Some(value) = self.comptime_values.get(name).cloned() {
            // v0.28.21 — `comptime func` items are folded at codegen-start
            // and intentionally not compiled to LLVM IR. Look up the
            // pre-computed value here and emit a constant in its place.
            // No-arg `comptime func` is the only supported shape.
            if !metadata_args.is_empty() {
                return Err(format!(
                    "comptime function '{}' is no-arg only in v0.28.21; got {} args",
                    name,
                    metadata_args.len()
                )
                .into());
            }
            self.value_to_llvm_const(&value)
        } else {
            let msg = if self.comptime_func_names.contains(name) {
                format!("comptime function '{}' is compile-time only; its body could not be folded (missing from comptime_values cache)", name)
            } else {
                format!("undefined function '{}' in codegen", name)
            };
            Err(msg.into())
        }
    }

    /// Emit a direct call to a known function, clear callback TLS, and record async info.
    fn emit_function_call(
        &mut self,
        function: inkwell::values::FunctionValue<'ctx>,
        name: &str,
        metadata_args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let call = self.build_call(function, metadata_args, "call")?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let null_i8 = i8_ptr_ty.const_null();
        let tls_ptrs: Vec<_> = self.pending_callback_tls.drain(..).collect();
        for tls_ptr in tls_ptrs {
            self.build_store(tls_ptr, null_i8)?;
        }
        if let Some(fdef) = self.func_defs.get(name) {
            if fdef.is_async {
                if let Some(ret_ty) = &fdef.ret {
                    if let Some(llvm_ret) = self.llvm_type_for(ret_ty) {
                        self.pending_spawn_type = Some(llvm_ret);
                    }
                }
            }
        }
        let result = call_try_basic_value(&call)
            .unwrap_or(self.context.i64_type().const_int(0, false).into());
        // CLOSE-GAP-5: when the callee returns a heap-owned `string` struct,
        // store it into a fresh alloca so the caller's `free_heap_allocs`
        // can release the data at scope exit. The callee already ensures the
        // data pointer is heap-owned (via `claim_string_return_value`), so
        // the registered pointer is always safe to free.
        let result = self.track_string_return_lifetime(name, result)?;
        // B9 (audit): same ownership transfer for closures — when the callee
        // returns a `func(...) -> ...` value, register its env so the
        // caller's scope exit releases it (the callee claimed it on return).
        let result = self.track_closure_return_lifetime(name, result)?;
        // L6: same ownership transfer for custom-enum payload boxes — when the
        // callee returns a custom enum, register its payload box (tag-conditional
        // free) so the caller's scope exit releases it (the callee claimed it on
        // return via claim_returned_enum_box).
        self.track_enum_box_return_lifetime(name, result)
    }

    /// If `result` is a Mimi string struct returned by a function call, stash
    /// it into a fresh alloca so the heap-owned data pointer can be freed at
    /// the caller's scope exit. Non-string or non-struct results pass through.
    fn track_string_return_lifetime(
        &self,
        callee_name: &str,
        result: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ret_is_string = self
            .func_defs
            .get(callee_name)
            .and_then(|fd| fd.ret.as_ref())
            .map(|t| matches!(t.unlocated(), Type::Name(n, _) if n == "string"))
            .unwrap_or(false);
        if !ret_is_string {
            return Ok(result);
        }
        let sv = match result {
            BasicValueEnum::StructValue(sv) => sv,
            other => return Ok(other),
        };
        let sty = sv.get_type();
        let fields = sty.get_field_types();
        let is_mimi_string_struct = fields.len() == 2
            && matches!(fields[0], BasicTypeEnum::PointerType(_))
            && matches!(fields[1], BasicTypeEnum::IntType(_));
        if !is_mimi_string_struct {
            return Ok(sv.into());
        }
        // Allocate a slot for the struct in the entry block, store into it,
        // register the data slot so the loader sees the latest value at free
        // time. Return the loaded struct to the caller.
        let slot = self.build_entry_alloca(sty, "call_str_slot")?;
        self.build_store(slot, sv)?;
        if self
            .gep()
            .build_struct_gep(sty, slot, 0, "call_str_data_gep")
            .is_ok()
        {
            self.register_heap_slot(slot, sty, 0);
        }
        let loaded = self.build_load(sty, slot, "call_str_load")?;
        Ok(loaded.into_struct_value().into())
    }

    /// B9 (audit): when the callee returns a closure (`func(...) -> ...`),
    /// register its env pointer so the caller's `free_heap_allocs` releases
    /// it at scope exit. Mirrors `track_string_return_lifetime`: the struct
    /// is stored into an entry alloca and the env field (field 1) is tracked
    /// as a heap slot. Non-closure results pass through unchanged.
    fn track_closure_return_lifetime(
        &self,
        callee_name: &str,
        result: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ret_is_closure = self
            .func_defs
            .get(callee_name)
            .and_then(|fd| fd.ret.as_ref())
            .map(|t| matches!(t.unlocated(), Type::Func(_, _)))
            .unwrap_or(false);
        if !ret_is_closure {
            return Ok(result);
        }
        let sv = match result {
            BasicValueEnum::StructValue(sv) => sv,
            other => return Ok(other),
        };
        let sty = sv.get_type();
        let fields = sty.get_field_types();
        let is_closure_struct = fields.len() == 2
            && matches!(fields[0], BasicTypeEnum::PointerType(_))
            && matches!(fields[1], BasicTypeEnum::PointerType(_));
        if !is_closure_struct {
            return Ok(sv.into());
        }
        // Allocate a slot for the struct in the entry block, store into it,
        // register the env field so the loader sees the latest value at free
        // time (and null-init makes never-allocated paths free(null) no-ops).
        let slot = self.build_entry_alloca(sty, "call_closure_slot")?;
        self.build_store(slot, sv)?;
        if self
            .gep()
            .build_struct_gep(sty, slot, 1, "call_closure_env_gep")
            .is_ok()
        {
            self.register_heap_slot(slot, sty, 1);
        }
        let loaded = self.build_load(sty, slot, "call_closure_load")?;
        Ok(loaded.into_struct_value().into())
    }

    /// L6: when the callee returns a custom enum, register its payload box for a
    /// tag-conditional free at the caller's scope exit (`HeapEntry::EnumBox`).
    /// Mirrors `track_string_return_lifetime`: the struct is stored into an entry
    /// alloca and registered. The free is conditional on the runtime tag (only
    /// `PayloadKind::Packed` variants carry a box; `Single`/`None` store inline
    /// data that must NOT be freed). The callee claimed the box on return
    /// (`claim_returned_enum_box`), so it survives until the caller frees it.
    /// Non-enum callees (records, primitives, built-in Option/Result) pass
    /// through unchanged — detected via the callee's return-type AST so a
    /// `{i32, i64}`-shaped record is never mistaken for an enum box carrier.
    pub(in crate::codegen) fn track_enum_box_return_lifetime(
        &self,
        callee_name: &str,
        result: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ret_ty = self
            .func_defs
            .get(callee_name)
            .and_then(|fd| fd.ret.as_ref());
        self.register_enum_box_for_return(ret_ty, result)
    }

    /// L6: register a returned custom-enum payload box for a tag-conditional
    /// free at the caller's scope exit, given the callee's return-type AST.
    /// Shared by named-function calls (`track_enum_box_return_lifetime`, which
    /// looks the return type up in `func_defs`) and closure calls (which pass
    /// the closure's declared return type directly). Non-enum return types
    /// (records, primitives, built-in Option/Result) pass through unchanged —
    /// `boxed_ordinals_for_return_type` returns `None` for them so a
    /// `{i32, i64}`-shaped record is never mistaken for an enum box carrier.
    pub(in crate::codegen) fn register_enum_box_for_return(
        &self,
        ret_ty: Option<&crate::ast::Type>,
        result: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let Some(boxed_ordinals) = self.boxed_ordinals_for_return_type(ret_ty) else {
            return Ok(result);
        };
        let sv = match result {
            BasicValueEnum::StructValue(sv) => sv,
            other => return Ok(other),
        };
        let sty = sv.get_type();
        if sty.count_fields() != 2 {
            return Ok(sv.into());
        }
        let slot = self.build_entry_alloca(sty, "call_enum_box_slot")?;
        self.build_store(slot, sv)?;
        self.register_enum_box(slot, sty, boxed_ordinals);
        let loaded = self.build_load(sty, slot, "call_enum_box_load")?;
        Ok(loaded.into_struct_value().into())
    }

    /// Build a call to a declared function and extract its basic value.
    pub(in crate::codegen) fn emit_direct_call(
        &self,
        function: inkwell::values::FunctionValue<'ctx>,
        compiled_args: &[BasicValueEnum<'ctx>],
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Coerce each arg to the declared param type: int width, string wrap
        // (raw i8* → {ptr,len}), list/record alloca load → by-value struct.
        let adjusted_args: Vec<BasicValueEnum<'ctx>> = compiled_args
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if let Some(param) = function.get_nth_param(i as u32) {
                    self.coerce_value_to_expected_type(*v, param.get_type())
                } else {
                    Ok(*v)
                }
            })
            .collect::<Result<_, CompileError>>()?;
        let metadata_args: Vec<_> = adjusted_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();
        let call = self.build_call(function, &metadata_args, name)?;
        Ok(call_try_basic_value(&call)
            .unwrap_or(self.context.i64_type().const_int(0, false).into()))
    }

    /// Codegen for `read_lines_each(path, callback)`.
    ///
    /// Runtime `mimi_read_lines_each` expects `void (*)(const char*)` — not a
    /// Mimi closure. Build a thin C thunk that:
    /// 1. Loads TLS-stored Mimi closure `{fn_ptr, env_ptr}`
    /// 2. Wraps the C line pointer into `{ptr, len}` via strlen
    /// 3. Calls `fn_ptr(env, string_struct)` (Mimi lambda ABI)
    pub(in crate::codegen) fn compile_read_lines_each_call(
        &mut self,
        compiled_args: &[BasicValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if compiled_args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "read_lines_each expects 2 arguments (path, callback)".into(),
            ));
        }
        let path_ptr = match compiled_args[0] {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::StructValue(sv) => {
                // Mimi string {ptr, len} — extract data pointer.
                self.builder
                    .build_extract_value(sv, 0, "rle_path_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract path: {}", e)))?
                    .into_pointer_value()
            }
            _ => {
                return Err(CompileError::Generic(
                    "read_lines_each: path must be string".into(),
                ))
            }
        };

        let closure_sv = match compiled_args[1] {
            BasicValueEnum::StructValue(sv) => sv,
            _ => {
                return Err(CompileError::Generic(
                    "read_lines_each: callback must be a closure".into(),
                ))
            }
        };
        let fn_ptr = self
            .builder
            .build_extract_value(closure_sv, 0, "rle_fn_ptr")
            .map_err(|e| CompileError::LlvmError(format!("extract fn: {}", e)))?
            .into_pointer_value();
        let env_ptr = self
            .builder
            .build_extract_value(closure_sv, 1, "rle_env_ptr")
            .map_err(|e| CompileError::LlvmError(format!("extract env: {}", e)))?
            .into_pointer_value();

        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        // TLS globals for this call site (reused pattern from callback thunks).
        let id = self.callback_thunk_counter;
        self.callback_thunk_counter += 1;
        let fn_global = self
            .module
            .add_global(i8_ptr, None, &format!("__mimi_rle_fnptr_{}", id));
        fn_global.set_initializer(&i8_ptr.const_null());
        fn_global.set_thread_local(true);
        fn_global.set_thread_local_mode(Some(inkwell::ThreadLocalMode::GeneralDynamicTLSModel));
        let env_global = self
            .module
            .add_global(i8_ptr, None, &format!("__mimi_rle_envptr_{}", id));
        env_global.set_initializer(&i8_ptr.const_null());
        env_global.set_thread_local(true);
        env_global.set_thread_local_mode(Some(inkwell::ThreadLocalMode::GeneralDynamicTLSModel));

        self.build_store(fn_global.as_pointer_value(), fn_ptr)?;
        self.build_store(env_global.as_pointer_value(), env_ptr)?;
        self.pending_callback_tls.push(fn_global.as_pointer_value());
        self.pending_callback_tls
            .push(env_global.as_pointer_value());

        // Build void(i8*) thunk if not already present for this id.
        let thunk_name = format!("__mimi_rle_thunk_{}", id);
        let void_ty = self.context.void_type();
        let thunk_fn_ty = void_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
        let thunk_fn = self.module.add_function(
            &thunk_name,
            thunk_fn_ty,
            Some(inkwell::module::Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);

        let line_c = thunk_fn
            .get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("rle thunk missing line param".into()))?
            .into_pointer_value();
        let tls_fn = self
            .build_load(i8_ptr, fn_global.as_pointer_value(), "rle_tls_fn")?
            .into_pointer_value();
        let tls_env = self
            .build_load(i8_ptr, env_global.as_pointer_value(), "rle_tls_env")?
            .into_pointer_value();

        // Wrap C string as Mimi {ptr, len} without alloca (SSA only).
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(line_c)],
                "rle_strlen",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen void".into()))?
            .into_int_value();
        let str_with_ptr = self
            .builder
            .build_insert_value(string_ty.get_undef(), line_c, 0, "rle_str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("rle str ptr: {}", e)))?
            .into_struct_value();
        let str_val = self
            .builder
            .build_insert_value(str_with_ptr, len, 1, "rle_str_len")
            .map_err(|e| CompileError::LlvmError(format!("rle str len: {}", e)))?
            .into_struct_value();

        // Mimi lambda ABI: fn(env_ptr, string) -> i64 (ignore return).
        let mimi_fn_ty = i64_ty.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                types::basic_to_metadata(self.context, BasicTypeEnum::StructType(string_ty)),
            ],
            false,
        );
        let fn_typed = self.build_pointer_cast(
            tls_fn,
            self.context.ptr_type(inkwell::AddressSpace::default()),
            "rle_fn_typed",
        )?;
        let _ = self
            .builder
            .build_indirect_call(
                mimi_fn_ty,
                fn_typed,
                &[
                    BasicMetadataValueEnum::PointerValue(tls_env),
                    BasicMetadataValueEnum::StructValue(str_val),
                ],
                "rle_cb_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("rle cb call: {}", e)))?;
        self.build_return(None)?;

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        let runtime_fn = self.get_runtime_fn("mimi_read_lines_each")?;
        let thunk_ptr = thunk_fn.as_global_value().as_pointer_value();
        let call = self.build_call(
            runtime_fn,
            &[
                BasicMetadataValueEnum::PointerValue(path_ptr),
                BasicMetadataValueEnum::PointerValue(thunk_ptr),
            ],
            "read_lines_each",
        )?;
        // Clear TLS after the call (same as other callback builtins).
        let tls_ptrs: Vec<_> = self.pending_callback_tls.drain(..).collect();
        for p in tls_ptrs {
            self.build_store(p, i8_ptr.const_null())?;
        }
        Ok(call_try_basic_value(&call).unwrap_or(i64_ty.const_int(0, false).into()))
    }

    pub(in crate::codegen) fn compile_call_mangled(
        &mut self,
        mangled: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let compiled_args = self.compile_arg_values(args, vars)?;
        // Adjust integer arg widths to match declared parameter types.
        let function = self.module.get_function(mangled);
        let adjusted_args: Vec<BasicValueEnum<'ctx>> = if let Some(f) = function {
            compiled_args
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    if let Some(param) = f.get_nth_param(i as u32) {
                        if let (
                            BasicValueEnum::IntValue(arg_iv),
                            BasicValueEnum::IntValue(param_iv),
                        ) = (*v, param)
                        {
                            let arg_bw = arg_iv.get_type().get_bit_width();
                            let param_bw = param_iv.get_type().get_bit_width();
                            if arg_bw == param_bw {
                                Ok(*v)
                            } else if arg_bw > param_bw {
                                Ok(self
                                    .builder
                                    .build_int_truncate(
                                        arg_iv,
                                        param_iv.get_type(),
                                        &format!("call_arg_trunc_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("arg trunc: {}", e))
                                    })?
                                    .into())
                            } else {
                                Ok(self
                                    .builder
                                    .build_int_s_extend(
                                        arg_iv,
                                        param_iv.get_type(),
                                        &format!("call_arg_sext_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("arg s_ext: {}", e))
                                    })?
                                    .into())
                            }
                        } else {
                            Ok(*v)
                        }
                    } else {
                        Ok(*v)
                    }
                })
                .collect::<Result<_, CompileError>>()?
        } else {
            compiled_args
        };
        let metadata_args: Vec<_> = adjusted_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();

        if let Some(function) = self.module.get_function(mangled) {
            let call = self.build_call(function, &metadata_args, "call")?;
            Ok(call_try_basic_value(&call)
                .unwrap_or(self.context.i64_type().const_int(0, false).into()))
        } else {
            let msg = if self.comptime_func_names.contains(mangled) {
                format!("comptime function '{}' is compile-time only and cannot be called from runtime code", mangled)
            } else {
                format!("undefined function '{}' in codegen", mangled)
            };
            Err(msg.into())
        }
    }

    /// Compile argument expressions into LLVM basic values.
    /// Named args (`name = expr`) are reordered to the function's parameter
    /// order when `func_name` is known (and present in `func_defs`).
    fn compile_arg_values(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<Vec<BasicValueEnum<'ctx>>, CompileError> {
        let mut compiled = Vec::with_capacity(args.len());
        for arg in args {
            let val = match arg.unlocated() {
                Expr::NamedArg(_, value) => self.compile_expr(value, vars)?,
                other => self.compile_expr(other, vars)?,
            };
            // 0.35.37 (exactly-once alignment): the CFG checker consumes a
            // capability passed as a call argument (Move semantics — see
            // resource_lower.rs capability_places / emit_consumes on Call
            // arguments), recursively through Tuple/List/Set/Record/Project
            // values. The legacy emitter never marked arguments consumed, so
            // `sink(c)` or `sink([c])` left `c` registered and codegen
            // demanded an extra drop(c) the checker did not require —
            // valid programs failed to compile. Collect every capability
            // variable reachable from the argument and mark it consumed,
            // mirroring the checker.
            let mut places = Vec::new();
            Self::collect_arg_cap_places(arg, vars, &mut places);
            for name in places {
                if self.is_cap_var(&name) && !self.is_cap_consumed(&name) {
                    self.consume_cap(&name)?;
                }
            }
            compiled.push(val);
        }
        Ok(compiled)
    }

    /// Mirror of `resource_lower.rs::collect_capability_places`: collect
    /// capability variable names reachable from an argument expression
    /// (Ident, and recursively through tuple/list/set/record literals and
    /// projections). Used to align call-argument consumption with the
    /// checker's Move semantics.
    pub(in crate::codegen) fn collect_arg_cap_places(
        arg: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
        out: &mut Vec<String>,
    ) {
        match arg.unlocated() {
            Expr::Ident(name) => {
                if vars.contains_key(name) {
                    out.push(name.clone());
                }
            }
            Expr::NamedArg(_, value) => Self::collect_arg_cap_places(value, vars, out),
            Expr::Tuple(values) => {
                for v in values {
                    Self::collect_arg_cap_places(v, vars, out);
                }
            }
            Expr::List(values) => {
                for v in values {
                    Self::collect_arg_cap_places(v, vars, out);
                }
            }
            Expr::SetLiteral(values) => {
                for v in values {
                    Self::collect_arg_cap_places(v, vars, out);
                }
            }
            Expr::Record { fields, .. } => {
                for field in fields {
                    Self::collect_arg_cap_places(&field.value, vars, out);
                }
            }
            Expr::Field(obj, _) => Self::collect_arg_cap_places(obj, vars, out),
            Expr::Index(base, index) => {
                Self::collect_arg_cap_places(base, vars, out);
                Self::collect_arg_cap_places(index, vars, out);
            }
            _ => {}
        }
    }

    /// Reorder named args to positional order for a known function definition.
    fn reorder_named_args(&self, name: &str, args: &[Expr]) -> Result<Vec<Expr>, CompileError> {
        let has_named = args
            .iter()
            .any(|a| matches!(a.unlocated(), Expr::NamedArg(_, _)));
        if !has_named {
            return Ok(args.to_vec());
        }
        let Some(fdef) = self.func_defs.get(name) else {
            // Unknown function (builtin/method): strip NamedArg wrappers only.
            // Match on `unlocated()` — Span/Origin (v0.31.1) wraps args in
            // `Expr::Located`, so a raw `Expr::NamedArg` match would miss them
            // and leak an unresolved NamedArg into compile_expr.
            return Ok(args
                .iter()
                .map(|a| match a.unlocated() {
                    Expr::NamedArg(_, v) => *v.clone(),
                    other => other.clone(),
                })
                .collect());
        };
        let mut ordered: Vec<Option<Expr>> = vec![None; fdef.params.len()];
        let mut next_pos = 0usize;
        for arg in args {
            // Match on `unlocated()` for the same reason as above: a Located
            // wrapper around a NamedArg must still be recognized and reordered.
            match arg.unlocated() {
                Expr::NamedArg(n, val) => {
                    let Some(pos) = fdef.params.iter().position(|p| p.name == *n) else {
                        return Err(CompileError::Generic(format!(
                            "unknown named argument '{}' for function '{}'",
                            n, name
                        )));
                    };
                    if pos >= ordered.len() {
                        ordered.resize(pos + 1, None);
                    }
                    ordered[pos] = Some(*val.clone());
                }
                other => {
                    while next_pos < ordered.len() && ordered[next_pos].is_some() {
                        next_pos += 1;
                    }
                    if next_pos >= ordered.len() {
                        ordered.push(Some(other.clone()));
                    } else {
                        ordered[next_pos] = Some(other.clone());
                    }
                    next_pos += 1;
                }
            }
        }
        // Fill defaults for missing slots.
        for (i, p) in fdef.params.iter().enumerate() {
            if i < ordered.len() && ordered[i].is_none() {
                if let Some(ref d) = p.default_value {
                    ordered[i] = Some(d.clone());
                }
            }
        }
        ordered
            .into_iter()
            .enumerate()
            .map(|(i, o)| {
                o.ok_or_else(|| {
                    CompileError::Generic(format!(
                        "missing argument {} for function '{}'",
                        i + 1,
                        name
                    ))
                })
            })
            .collect()
    }

    /// Lower `view`/`mutate` user-function arguments to the reference ABI.
    /// Scalar/struct locals pass their alloca; List literals already evaluate
    /// to a pointer to the authoritative List header and pass that pointer.
    fn prepare_borrowed_user_args(
        &mut self,
        name: &str,
        arg_exprs: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(fdef) = self.func_defs.get(name).cloned() else {
            return Ok(());
        };
        for (index, param) in fdef.params.iter().enumerate() {
            if param.borrow.is_none() || index >= args.len() || index >= arg_exprs.len() {
                continue;
            }
            // v0.31.6: unlocate the argument — v0.31.1 wraps call args in
            // Expr::Located, so matching `&arg_exprs[index]` missed the
            // Ident/Field arms and fell through to the rvalue arm, which
            // materialized a temporary. A `mutate List` param then received a
            // pointer to a throwaway copy, so in-callee push/pop never reached
            // the caller's list (stale len → FFI index OOB).
            match arg_exprs[index].unlocated() {
                Expr::Ident(var_name) => {
                    let Some(&(slot, stored_ty)) = vars.get(var_name) else {
                        return Err(CompileError::Generic(format!(
                            "borrowed argument '{}' must refer to a local variable",
                            var_name
                        )));
                    };
                    // v0.31.6: unlocate param.ty — v0.31.1 wraps parameter types
                    // in Type::Located, so matching `&param.ty` against
                    // `Type::Name("List", _)` missed, list_is_already_indirect
                    // stayed false, and the else branch passed the var *slot*
                    // (a pointer-to-pointer) instead of loading the authoritative
                    // list pointer. push then realloc'd a garbage data field.
                    //
                    // v0.31.25: extend to ALL indirect vars (records, lists, etc.).
                    // When stored_ty is a pointer, the alloca holds a pointer to
                    // the actual data. Load it to pass the data pointer, not the
                    // pointer-to-pointer.
                    let is_already_indirect = matches!(stored_ty, BasicTypeEnum::PointerType(_));
                    if is_already_indirect {
                        args[index] =
                            self.build_load(stored_ty, slot, &format!("{}_borrow_ptr", var_name))?;
                    } else {
                        args[index] = BasicValueEnum::PointerValue(slot);
                    }
                }
                Expr::Field(object, field_name) => {
                    let field_slot = self.compile_field_gep(object, field_name, vars)?;
                    args[index] = BasicValueEnum::PointerValue(field_slot);
                }
                _ => {
                    let target_ty = self.llvm_type_for(&param.ty).ok_or_else(|| {
                        CompileError::Generic(format!(
                            "no LLVM type for borrowed argument {}",
                            index + 1
                        ))
                    })?;
                    // Rvalues have no caller storage. Materialize a temporary
                    // so `view 5` and `mutate 7` remain ergonomic; mutations to
                    // a temporary are intentionally observable only through the
                    // function's return value.
                    if matches!(args[index], BasicValueEnum::PointerValue(_))
                        && matches!(target_ty, BasicTypeEnum::StructType(_))
                    {
                        // Aggregate literals such as List already evaluate to
                        // a pointer to their authoritative temporary storage.
                    } else {
                        let slot = self.build_alloca(target_ty, "borrowed_temp")?;
                        let value = self.adjust_int_val(args[index], target_ty)?;
                        self.build_store(slot, value)?;
                        args[index] = BasicValueEnum::PointerValue(slot);
                    }
                }
            }
        }
        Ok(())
    }

    /// Convert compiled arguments to the declared parameter types of a user-defined
    /// function. This mirrors the interpreter's implicit numeric coercion, so
    /// calls like `power(2, 10)` (where `power` expects `f64`) pass `2.0` and
    /// `10.0` to the generated function.
    fn coerce_args_to_param_types(
        &self,
        name: &str,
        args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let fdef = if let Some(f) = self.func_defs.get(name) {
            f.clone()
        } else {
            return Ok(());
        };
        // Generic functions: skip coercion here. The outer type_map cannot
        // resolve generic params (llvm_type_for falls back to i64, so `0.0`
        // would be fptosi'd to i64 0 before monomorphization). The monomorph
        // call site re-coerces against the concrete function's parameter
        // types (see coerce_args_to_function).
        if !fdef.generics.is_empty() {
            return Ok(());
        }
        for (i, param) in fdef.params.iter().enumerate() {
            if i >= args.len() {
                break;
            }
            if param.borrow.is_some() {
                continue;
            }
            if let Some(target) = self.llvm_type_for(&param.ty) {
                args[i] = self.adjust_int_val(args[i], target)?;
            }
        }
        Ok(())
    }

    /// Coerce already-compiled call args to a concrete function's parameter
    /// types (used at monomorphized call sites where generic params are
    /// resolved). Rebuilds the metadata arg list from the coerced values.
    fn coerce_args_to_function(
        &self,
        function: inkwell::values::FunctionValue<'ctx>,
        args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        for (i, arg) in args.iter_mut().enumerate() {
            if let Some(pt) = function.get_nth_param(i as u32) {
                let target = pt.get_type();
                // Deep-eval 2026-08-09 (demos/03 swap_pair compiler crash):
                // a string literal reaching a monomorphized generic string
                // parameter is still a raw C-string pointer; the callee
                // expects the Mimi string struct {ptr,i64}. Passing the bare
                // pointer is an ABI violation that LLVM SelectionDAG crashes
                // on. Wrap it before the call.
                if let BasicTypeEnum::StructType(st) = target {
                    let fields = st.get_field_types();
                    let is_string_struct = fields.len() == 2
                        && matches!(fields[0], BasicTypeEnum::PointerType(_))
                        && matches!(fields[1], BasicTypeEnum::IntType(_));
                    if is_string_struct
                        && matches!(arg, BasicValueEnum::PointerValue(_))
                        && arg.get_type() != target
                    {
                        let pv = match *arg {
                            BasicValueEnum::PointerValue(pv) => pv,
                            _ => unreachable!(),
                        };
                        *arg = self.wrap_c_string(pv)?;
                        continue;
                    }
                }
                *arg = self.adjust_int_val(*arg, target)?;
            }
        }
        Ok(())
    }

    /// Decide whether a user-defined function with the given name can plausibly
    /// accept these argument expressions. This is used when a builtin and a user
    /// function share a name (e.g. `contains`) to resolve the ambiguity created
    /// by flattening imported modules into a single namespace.
    fn user_func_signature_matches(&self, name: &str, args: &[Expr]) -> bool {
        let fdef = match self.func_defs.get(name) {
            Some(f) => f,
            None => return false,
        };
        for (i, param) in fdef.params.iter().enumerate() {
            if i >= args.len() {
                break;
            }
            let arg_ty = match self.expr_type_of(&args[i], &HashMap::new()) {
                Some(t) => t,
                None => continue,
            };
            // For concrete scalar parameter types, require an exact match.
            // Generic or complex parameter types are assumed compatible.
            let is_concrete_scalar = matches!(
                &param.ty,
                crate::ast::Type::Name(n, _)
                    if n == "string" || n == "i32" || n == "i64" || n == "f64" || n == "bool"
            );
            if is_concrete_scalar && arg_ty != param.ty {
                return false;
            }
        }
        true
    }

    /// Get or create a closure ABI wrapper for a named function.
    pub(in crate::codegen) fn get_or_create_closure_wrapper(
        &mut self,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        if let Some(cached) = self.closure_wrappers.get(name) {
            return Ok(*cached);
        }

        let orig_fn = self.module.get_function(name).ok_or_else(|| {
            CompileError::Generic(format!(
                "cannot create closure wrapper for unknown function '{}'",
                name
            ))
        })?;
        let fn_type = orig_fn.get_type();
        let param_tys = fn_type.get_param_types();
        let ret_ty = fn_type.get_return_type().ok_or_else(|| {
            CompileError::Generic(format!(
                "closure wrapper: function '{}' has void return type",
                name
            ))
        })?;

        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let mut wrapper_params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        wrapper_params.push(BasicMetadataTypeEnum::PointerType(i8_ptr));
        for pt in &param_tys {
            wrapper_params.push(*pt);
        }

        let wrapper_fn_type = fn_type_for_basic_type(ret_ty, &wrapper_params)?;
        let wrapper_name = format!("__mimi_fn_wrapper_{}", name.replace('.', "_"));
        let wrapper_fn = self.module.add_function(
            &wrapper_name,
            wrapper_fn_type,
            Some(inkwell::module::Linkage::Internal),
        );

        let saved_block = self.builder.get_insert_block();
        let entry_bb = self.context.append_basic_block(wrapper_fn, "entry");
        self.builder.position_at_end(entry_bb);

        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for i in 0..param_tys.len() {
            let param = wrapper_fn.get_nth_param((i + 1) as u32).ok_or_else(|| {
                CompileError::LlvmError(format!("wrapper: param {} not found", i + 1))
            })?;
            call_args.push(types::basic_value_to_metadata_value(
                &param,
                self.context.i64_type(),
            ));
        }

        let call = self.build_call(orig_fn, &call_args, "wrapper_call")?;
        let ret_val = crate::codegen::call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("wrapper call returned void".to_string()))?;
        self.build_return(Some(&ret_val))?;

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        let wrapper_ptr = wrapper_fn.as_global_value().as_pointer_value();
        self.closure_wrappers.insert(name.to_string(), wrapper_ptr);
        Ok(wrapper_ptr)
    }

    /// Serialize a `List<RecordType>` to JSON by generating a per-type element
    /// serializer function and calling `mimi_list_record_to_json` with a callback.
    pub(in crate::codegen) fn compile_record_list_to_json(
        &mut self,
        type_name: &str,
        fields: &[crate::ast::Field],
        list_alloca: &inkwell::values::PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // Create or reuse the element serializer function: i8*(i8*)
        let fn_name = format!("{}_to_json_elem", type_name);
        let elem_fn = if let Some(f) = self.module.get_function(&fn_name) {
            f
        } else {
            let fn_ty = i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
            let func =
                self.module
                    .add_function(&fn_name, fn_ty, Some(inkwell::module::Linkage::Internal));
            // Set up the function body
            let entry_bb = self.context.append_basic_block(func, "entry");
            // Save the current position
            let saved_block = self.builder.get_insert_block();
            self.builder.position_at_end(entry_bb);

            // Cast the input pointer to the struct type
            let llvm_ty = self.type_llvm[type_name];
            let BasicTypeEnum::StructType(sty) = llvm_ty else {
                return Err(CompileError::Generic(format!(
                    "type '{}' is not a struct; cannot create element function for list operation",
                    type_name
                )));
            };
            let typed_ptr = self
                .builder
                .build_bit_cast(
                    func.get_nth_param(0)
                        .ok_or_else(|| CompileError::Generic("elem fn missing param 0".into()))?
                        .into_pointer_value(),
                    i8_ptr_ty,
                    "typed_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))?
                .into_pointer_value();

            // Load the struct value
            let struct_val = self
                .builder
                .build_load(BasicTypeEnum::StructType(sty), typed_ptr, "elem_val")
                .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?
                .into_struct_value();

            // Sort fields alphabetically
            let mut idx_map: Vec<(usize, &crate::ast::Field)> = fields.iter().enumerate().collect();
            idx_map.sort_by(|a, b| a.1.name.cmp(&b.1.name));

            // Build format string and args
            let mut fmt = String::from("{");
            let mut sprintf_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
            for (pos, (i, field)) in idx_map.iter().enumerate() {
                if pos > 0 {
                    fmt.push(',');
                }
                let field_val = self
                    .builder
                    .build_extract_value(
                        inkwell::values::AggregateValueEnum::StructValue(struct_val),
                        *i as u32,
                        &field.name,
                    )
                    .map_err(|e| {
                        CompileError::LlvmError(format!("extract field {}: {}", field.name, e))
                    })?;
                match field.ty.unlocated() {
                    Type::Name(n, _) if n == "string" => {
                        fmt.push_str(&format!("\"{}\":\"%s\"", field.name));
                        let sv = field_val.into_struct_value();
                        let dp = self
                            .builder
                            .build_extract_value(
                                inkwell::values::AggregateValueEnum::StructValue(sv),
                                0,
                                &format!("{}_data", field.name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("extract str: {}", e)))?
                            .into_pointer_value();
                        sprintf_args.push(BasicMetadataValueEnum::PointerValue(dp));
                    }
                    Type::Name(n, _) if matches!(n.as_str(), "i32" | "i64") => {
                        fmt.push_str(&format!("\"{}\":%ld", field.name));
                        let iv = field_val.into_int_value();
                        if n == "i32" {
                            // A1: use s_extend for signed integers.
                            let bw = iv.get_type().get_bit_width();
                            let ext = if bw == 1 {
                                self.builder
                                    .build_int_z_extend(iv, i64_ty, &format!("{}_ext", field.name))
                                    .map_err(|e| CompileError::LlvmError(format!("zext: {}", e)))?
                            } else {
                                self.builder
                                    .build_int_s_extend(iv, i64_ty, &format!("{}_ext", field.name))
                                    .map_err(|e| CompileError::LlvmError(format!("sext: {}", e)))?
                            };
                            sprintf_args.push(BasicMetadataValueEnum::IntValue(ext));
                        } else {
                            sprintf_args.push(BasicMetadataValueEnum::IntValue(iv));
                        }
                    }
                    Type::Name(n, _) if n == "bool" => {
                        fmt.push_str(&format!("\"{}\":%s", field.name));
                        let iv = field_val.into_int_value();
                        let true_global = self
                            .builder
                            .build_global_string_ptr("true", &format!("{}_true", field.name))
                            .map_err(|e| CompileError::LlvmError(format!("true: {}", e)))?;
                        let false_global = self
                            .builder
                            .build_global_string_ptr("false", &format!("{}_false", field.name))
                            .map_err(|e| CompileError::LlvmError(format!("false: {}", e)))?;
                        let zero = self.context.bool_type().const_int(0, false);
                        let is_true = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                iv,
                                zero,
                                &format!("{}_is_true", field.name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                        let selected = self
                            .builder
                            .build_select(
                                is_true,
                                true_global.as_pointer_value(),
                                false_global.as_pointer_value(),
                                &format!("{}_json", field.name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("select: {}", e)))?;
                        sprintf_args.push(BasicMetadataValueEnum::PointerValue(
                            selected.into_pointer_value(),
                        ));
                    }
                    Type::Name(n, _) if n == "f64" => {
                        // RECORD-FLOAT-JSON-PARITY (0.39.x usability sweep, Round 29):
                        // shortest-round-trip via `mimi_to_string_f64` (same as the
                        // VM's serde_json and the standalone `to_json` FloatValue
                        // path), NOT `%g` (which emits `1` for `1.0`).
                        fmt.push_str(&format!("\"{}\":%s", field.name));
                        let fv = field_val.into_float_value();
                        let fstr_fn = self
                            .get_runtime_fn("mimi_to_json_f64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let fstr = self
                            .build_call(
                                fstr_fn,
                                &[BasicMetadataValueEnum::FloatValue(fv)],
                                &format!("{}_f64_json", field.name),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| {
                                CompileError::LlvmError("mimi_to_string_f64 returned void".into())
                            })?
                            .into_pointer_value();
                        sprintf_args.push(BasicMetadataValueEnum::PointerValue(fstr));
                    }
                    _ => {
                        return Err(CompileError::Generic(format!(
                            "unsupported field type {:?} for to_json",
                            field.ty
                        )));
                    }
                }
            }
            fmt.push('}');

            // Allocate buffer and sprintf (CG-H1: size from format + field slack).
            let est = (fmt.len() + fields.len() * 256 + 1024).max(4096) as u64;
            let buf_size = i64_ty.const_int(est, false);
            // B4: OOM-safe buffer for element to_json.
            let buf = self.malloc_or_abort(buf_size, "elem_json_malloc")?;
            let fmt_ptr = self
                .builder
                .build_global_string_ptr(&fmt, "elem_json_fmt")
                .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
            // B3/CG-C3: snprintf returns i32, not i8*.
            let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let i32_ty = self.context.i32_type();
                let ty = i32_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                    ],
                    true,
                );
                self.module
                    .add_function("snprintf", ty, Some(inkwell::module::Linkage::External))
            });
            let mut all_args = vec![BasicMetadataValueEnum::PointerValue(buf)];
            all_args.push(BasicMetadataValueEnum::IntValue(buf_size));
            all_args.push(BasicMetadataValueEnum::PointerValue(
                fmt_ptr.as_pointer_value(),
            ));
            all_args.extend(sprintf_args);
            self.build_call(snprintf_fn, &all_args, "elem_json_snprintf")?;
            // Return the buffer pointer
            let ret_val: BasicValueEnum<'ctx> = buf.into();
            self.builder
                .build_return(Some(&ret_val))
                .map_err(|e| CompileError::LlvmError(format!("ret: {}", e)))?;

            // Restore the saved position
            if let Some(bb) = saved_block {
                self.builder.position_at_end(bb);
            }

            func
        };

        // Call mimi_list_record_to_json(list_alloca, elem_fn)
        let helper_name = "mimi_list_record_to_json";
        let helper_fn = self.module.get_function(helper_name).unwrap_or_else(|| {
            let fn_ty = i8_ptr_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                ],
                false,
            );
            self.module
                .add_function(helper_name, fn_ty, Some(inkwell::module::Linkage::External))
        });
        let elem_fn_ptr = elem_fn.as_global_value().as_pointer_value();
        let raw = self
            .build_call(
                helper_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(*list_alloca),
                    BasicMetadataValueEnum::PointerValue(elem_fn_ptr),
                ],
                "to_json_record_list",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_list_record_to_json returned void")?
            .into_pointer_value();
        self.register_heap_alloc(raw);
        self.wrap_c_string(raw)
    }

    /// Find a FuncDef by name from the codegen's stored func_defs
    pub(in crate::codegen) fn find_func_def(&self, name: &str) -> Result<FuncDef, CompileError> {
        self.func_defs.get(name).cloned().ok_or_else(|| {
            CompileError::Generic(format!(
                "function '{}' definition not available for monomorphization",
                name
            ))
        })
    }

    /// Serialize a named Record at `struct_ptr` to a heap JSON C string.
    /// Caller owns the buffer (export) or should register_heap_alloc (to_json).
    pub(in crate::codegen) fn compile_record_to_json_cstr(
        &self,
        obj_type: &str,
        struct_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let type_def = self.type_defs.get(obj_type).ok_or_else(|| {
            CompileError::LlvmError(format!("no type def for record {}", obj_type))
        })?;
        let fields = match &type_def.kind {
            TypeDefKind::Record(fields) => fields.clone(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "{} is not a record",
                    obj_type
                )))
            }
        };
        let llvm_ty = *self.type_llvm.get(obj_type).ok_or_else(|| {
            CompileError::LlvmError(format!("no LLVM type for record {}", obj_type))
        })?;
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return Err(CompileError::LlvmError(format!(
                "record type {} is not a struct",
                obj_type
            )));
        };
        let i64_ty = self.context.i64_type();
        let mut idx_map: Vec<(usize, Field)> = fields.iter().cloned().enumerate().collect();
        idx_map.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        let mut fmt = String::from("{");
        let mut sprintf_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for (pos, (i, field)) in idx_map.iter().enumerate() {
            if pos > 0 {
                fmt.push(',');
            }
            let gep = self
                .gep()
                .build_struct_gep(sty, struct_ptr, *i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let ft = sty
                .get_field_type_at_index(*i as u32)
                .ok_or_else(|| CompileError::LlvmError("missing field type".into()))?;
            let field_val = self.build_load(ft, gep, &format!("load_{}", field.name))?;
            match field.ty.unlocated() {
                Type::Name(n, _) if n == "string" => {
                    fmt.push_str(&format!("\"{}\":\"%s\"", field.name));
                    let sv = field_val.into_struct_value();
                    let dp = self
                        .builder
                        .build_extract_value(
                            inkwell::values::AggregateValueEnum::StructValue(sv),
                            0,
                            &format!("{}_data", field.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_pointer_value();
                    sprintf_args.push(BasicMetadataValueEnum::PointerValue(dp));
                }
                Type::Name(n, _) if matches!(n.as_str(), "i32" | "i64") => {
                    fmt.push_str(&format!("\"{}\":%ld", field.name));
                    let field_iv = field_val.into_int_value();
                    let field_i64 = if field_iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(field_iv, self.context.i64_type(), "json_i32_ext")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        field_iv
                    };
                    sprintf_args.push(BasicMetadataValueEnum::IntValue(field_i64));
                }
                Type::Name(n, _) if n == "bool" => {
                    fmt.push_str(&format!("\"{}\":%s", field.name));
                    let iv = field_val.into_int_value();
                    let true_global = self
                        .builder
                        .build_global_string_ptr("true", &format!("{}_true", field.name))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let false_global = self
                        .builder
                        .build_global_string_ptr("false", &format!("{}_false", field.name))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let zero = self.context.bool_type().const_int(0, false);
                    let is_true = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            iv,
                            zero,
                            &format!("{}_is_true", field.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let selected = self
                        .builder
                        .build_select(
                            is_true,
                            true_global.as_pointer_value(),
                            false_global.as_pointer_value(),
                            &format!("{}_json", field.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    sprintf_args.push(BasicMetadataValueEnum::PointerValue(
                        selected.into_pointer_value(),
                    ));
                }
                Type::Name(n, _) if n == "f64" => {
                    // RECORD-FLOAT-JSON-PARITY (0.39.x usability sweep, Round 29):
                    // use the same shortest-round-trip formatter the VM's
                    // `value_to_json` (serde_json) and the standalone `to_json`
                    // FloatValue path use `mimi_to_json_f64` (serde shortest
                    // round-trip: `1.0` for whole numbers), NOT `mimi_to_string_f64`
                    // (which drops the `.0` and emits `1` for `1.0`).
                    fmt.push_str(&format!("\"{}\":%s", field.name));
                    let fv = field_val.into_float_value();
                    let fstr_fn = self
                        .get_runtime_fn("mimi_to_json_f64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let fstr = self
                        .build_call(
                            fstr_fn,
                            &[BasicMetadataValueEnum::FloatValue(fv)],
                            &format!("{}_f64_json", field.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| {
                            CompileError::LlvmError("mimi_to_string_f64 returned void".into())
                        })?
                        .into_pointer_value();
                    sprintf_args.push(BasicMetadataValueEnum::PointerValue(fstr));
                }
                // Nested record field: serialize recursively so `to_json`
                // matches the bytecode VM's `value_to_json` (which recurses into
                // nested structures). Handles `Box { a: Point }`.
                Type::Name(n, _) => {
                    if let Some(td) = self.type_defs.get(n.as_str()) {
                        if matches!(td.kind, TypeDefKind::Record(_)) {
                            fmt.push_str(&format!("\"{}\":%s", field.name));
                            let nested = self.compile_record_to_json_cstr(n, gep)?;
                            sprintf_args.push(BasicMetadataValueEnum::PointerValue(nested));
                            continue;
                        }
                    }
                    return Err(CompileError::Generic(format!(
                        "to_json: unsupported record field type for '{}' in {}",
                        field.name, obj_type
                    )));
                }
                _ => {
                    return Err(CompileError::Generic(format!(
                        "to_json: unsupported record field type for '{}' in {}",
                        field.name, obj_type
                    )))
                }
            }
        }
        fmt.push('}');
        let est = (fmt.len() + fields.len() * 256 + 1024).max(4096) as u64;
        let buf_size = i64_ty.const_int(est, false);
        let buf = self.malloc_or_abort(buf_size, "record_json_malloc")?;
        let fmt_ptr = self
            .builder
            .build_global_string_ptr(&fmt, "record_json_fmt")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let mut all_args = vec![BasicMetadataValueEnum::PointerValue(buf)];
        all_args.push(BasicMetadataValueEnum::IntValue(buf_size));
        all_args.push(BasicMetadataValueEnum::PointerValue(
            fmt_ptr.as_pointer_value(),
        ));
        all_args.extend(sprintf_args);
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module
                .add_function("snprintf", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(snprintf_fn, &all_args, "record_json_snprintf")?;
        Ok(buf)
    }

    // ===== Recursive `to_json` generator (true architectural fix, Phase A) =====
    //
    // One serializer per type: `i8* mimi_to_json_<san>(i8* slot)` where `slot`
    // is always a pointer to an `i64`. For slot-types the i64 holds the value
    // (or ptrtoint of a string/list pointer); for struct-types it holds the
    // struct address as a ptrtoint. This single convention eliminates the
    // 99 bespoke runtime functions and the per-combination dispatch tree.

    /// Stable, unique serializer function name for a type.
    fn json_type_name(ty: &crate::ast::Type) -> String {
        format!("{:?}", ty).replace(|c: char| !c.is_alphanumeric() && c != '_', "_")
    }

    /// Normalize a type for JSON serialization matching:
    /// - strip metadata-carrying `Located` wrappers,
    /// - convert surface `Name("Option", [..])` / `Name("Result", [..])` forms
    ///   (as stored in `type_defs`) into the canonical `Type::Option` /
    ///   `Type::Result` variants (as produced by `parse_type_str`).
    fn json_norm(&self, ty: &crate::ast::Type) -> crate::ast::Type {
        match ty {
            crate::ast::Type::Located { ty, .. } => self.json_norm(ty),
            crate::ast::Type::Name(n, args) => match n.as_str() {
                "Option" if args.len() == 1 => {
                    crate::ast::Type::Option(Box::new(self.json_norm(&args[0])))
                }
                "Result" if args.len() == 2 => crate::ast::Type::Result(
                    Box::new(self.json_norm(&args[0])),
                    Box::new(self.json_norm(&args[1])),
                ),
                "List" if args.len() == 1 => {
                    crate::ast::Type::Name("List".to_string(), vec![self.json_norm(&args[0])])
                }
                _ => crate::ast::Type::Name(
                    n.clone(),
                    args.iter().map(|a| self.json_norm(a)).collect(),
                ),
            },
            crate::ast::Type::Option(inner) => {
                crate::ast::Type::Option(Box::new(self.json_norm(inner)))
            }
            crate::ast::Type::Result(a, b) => crate::ast::Type::Result(
                Box::new(self.json_norm(a)),
                Box::new(self.json_norm(b)),
            ),
            crate::ast::Type::Tuple(es) => {
                crate::ast::Type::Tuple(es.iter().map(|e| self.json_norm(e)).collect())
            }
            other => other.clone(),
        }
    }

    /// Is this type laid out as an inline struct (needs an address in the slot)?
    fn json_is_struct_type(&self, ty: &crate::ast::Type) -> bool {
        let nty = self.json_norm(ty);
        let ty = &nty;
        match ty {
            crate::ast::Type::Option(_) | crate::ast::Type::Result(_, _) => true,
            crate::ast::Type::Tuple(_) => true,
            crate::ast::Type::Name(n, _) => {
                if n == "Option" || n == "Result" {
                    true
                } else {
                    self.type_defs.get(n).map_or(false, |td| {
                        matches!(
                            td.kind,
                            crate::ast::TypeDefKind::Record(_) | crate::ast::TypeDefKind::Enum(_)
                        )
                    })
                }
            }
            _ => false,
        }
    }

    /// Is `ty` a leaf that is always stored inline by value (a numeric/char/bool
    /// scalar or a `string` struct)? Such payloads are never heap-packed inside
    /// an `Option`/`Result`, so the simple `json_ser_field_call` path is correct.
    /// Everything else (List/Set/Map/Record/Tuple/Option/Result payloads) may be
    /// stored either embedded or heap-packed, and needs the runtime probe.
    fn json_is_scalar_or_string(&self, ty: &crate::ast::Type) -> bool {
        let nty = self.json_norm(ty);
        match &nty {
            crate::ast::Type::Name(n, _) => matches!(
                n.as_str(),
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "char"
                    | "bool" | "f32" | "f64" | "string"
            ),
            _ => false,
        }
    }

    /// The *native-width* LLVM storage type for a scalar element of a
    /// `List<scalar>`. The legacy emitter widens narrow ints (`i8`/`i16`/`i32`/
    /// `u8`/…) and `f32` to `i64`/`f64` in `llvm_type_for`, but the list data
    /// array keeps elements at their true width — so the per-element serializer
    /// must read the narrow width and the iteration stride must match it. The
    /// resolved emitter already lowers these to their native width, so this
    /// helper is a no-op there and purely a legacy-layout correction.
    fn json_native_scalar_llvm(&self, ty: &crate::ast::Type) -> Option<BasicTypeEnum<'ctx>> {
        let nty = self.json_norm(ty);
        match nty.unlocated() {
            crate::ast::Type::Name(n, _) => match n.as_str() {
                "i8" | "u8" => Some(self.context.i8_type().into()),
                "i16" | "u16" => Some(self.context.i16_type().into()),
                "i32" | "u32" => Some(self.context.i32_type().into()),
                "f32" => Some(self.context.f32_type().into()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Load a (possibly narrowed) integer-like slot as a full `i64` for JSON
    /// emission. The slot storage width follows `actual_ty`:
    /// * `Some(IntType(bits))` with `bits < 64` → load that width and sign/zero
    ///   extend to `i64` (e.g. an `i32` element of `List<i32>` is stored as a raw
    ///   4-byte value, not a widened `i64`).
    /// * otherwise (widened payload slots, top-level i64-padded args) → `load i64`.
    fn json_load_int_as_i64(
        &mut self,
        slot: inkwell::values::PointerValue<'ctx>,
        actual_ty: Option<BasicTypeEnum<'ctx>>,
        signed: bool,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        if let Some(BasicTypeEnum::IntType(it)) = actual_ty {
            let bw = it.get_bit_width();
            if bw < 64 {
                let raw = self
                    .build_load(BasicTypeEnum::IntType(it), slot, "json_ld_w")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                return self
                    .builder
                    .build_int_cast_sign_flag(raw, i64_ty, signed, "json_iscalar")
                    .map_err(|e| CompileError::LlvmError(e.to_string()));
            }
        }
        self.build_load(BasicTypeEnum::IntType(i64_ty), slot, "json_ld")
            .map_err(|e| CompileError::LlvmError(e.to_string()))
            .map(|v| v.into_int_value())
    }

    /// Load a float slot as the `f64` bit pattern in an `i64`. A narrowed `f32`
    /// element of `List<f32>` is stored as a raw 4-byte value: load `f32`,
    /// extend to `f64`, and bitcast to `i64` so `mimi_json_f64_to_string` sees a
    /// valid `f64` bit pattern.
    fn json_load_float_as_i64(
        &mut self,
        slot: inkwell::values::PointerValue<'ctx>,
        actual_ty: Option<BasicTypeEnum<'ctx>>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        if let Some(BasicTypeEnum::FloatType(ft)) = actual_ty {
            if ft.get_bit_width() == 32 {
                let raw = self
                    .build_load(BasicTypeEnum::FloatType(ft), slot, "json_ld_f")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_float_value();
                let f64v = self
                    .builder
                    .build_float_ext(raw, self.context.f64_type(), "json_fext")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                return self
                    .builder
                    .build_bit_cast(f64v, i64_ty, "json_fbits")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))
                    .map(|v| v.into_int_value());
            }
        }
        self.build_load(BasicTypeEnum::IntType(i64_ty), slot, "json_ld")
            .map_err(|e| CompileError::LlvmError(e.to_string()))
            .map(|v| v.into_int_value())
    }

    /// Does `ty`, when stored as a *payload* of an enclosing `Option`/`Result`, get
    /// heap-boxed (the `{disc, box_ptr, payload}` external form, with the inner
    /// aggregate itself stored reversed as `{value, box_ptr}`)?  This happens for
    /// `Option`/`Result` whose payload values hold a scalar (e.g. `Option<i64>`,
    /// `Result<i64,string>`); container payloads (`List`, `Map`, `Set`, records,
    /// tuples) are stored inline instead.  A bare scalar/string is never boxed.
    fn json_is_boxable(&self, ty: &crate::ast::Type) -> bool {
        let nty = self.json_norm(ty);
        match &nty {
            crate::ast::Type::Option(inner) => self.json_holds_scalar(inner),
            crate::ast::Type::Result(ok, _) => self.json_holds_scalar(ok),
            _ => false,
        }
    }

    /// Does `ty` transitively carry a scalar/string value (used to decide boxing)?
    fn json_holds_scalar(&self, ty: &crate::ast::Type) -> bool {
        let nty = self.json_norm(ty);
        match &nty {
            crate::ast::Type::Name(n, _) => matches!(
                n.as_str(),
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "char"
                    | "bool" | "f32" | "f64" | "string"
            ),
            crate::ast::Type::Option(inner) => self.json_holds_scalar(inner),
            crate::ast::Type::Result(ok, err) => {
                self.json_holds_scalar(ok) || self.json_holds_scalar(err)
            }
            _ => false,
        }
    }

    /// When `inner` is the payload of an `Option`/`Result`, does it get heap-boxed
    /// (stored reversed as `{value, box_ptr}`)? This is true only for an `Option`/
    /// `Result` *aggregate* whose payload holds a scalar — i.e. `Option<i64>`,
    /// `Result<i64,…>`, `Option<Option<i64>>`, etc. A plain scalar inner (`i64`)
    /// or a container/record/tuple inner is stored inline, not boxed.
    fn json_inner_boxable(&self, inner: &crate::ast::Type) -> bool {
        let nty = self.json_norm(inner);
        match nty.unlocated() {
            crate::ast::Type::Option(_) | crate::ast::Type::Result(_, _) => {
                self.json_holds_scalar(inner)
            }
            _ => false,
        }
    }

    /// The *runtime storage layout* of a Mimi value as stored inside an
    /// `Option<T>` / `Result<T, E>` payload slot (or as a list element / record
    /// field).  This deliberately mirrors the actual ABI the compiler emits,
    /// which for `Option<T>` / `Result<T, E>` is **not** `{i1 disc, payload}`:
    /// the discriminant lives in the low bit of an 8-byte tagged pointer at
    /// field 0, and the payload begins at offset 8.  `llvm_type_for` instead
    /// returns a force-heap `{i1, i64}` shape whose field-1 offset (4 on this
    /// target) does not match the runtime, which is what produced the
    /// misaligned-list crash for nested `Option<List>` / `Result<Option<List>>`.
    ///
    /// We recurse so that e.g. `Option<Option<List>>` and
    /// `Result<Option<List>, string>` get the correct nested offsets for both
    /// the discriminant (field 0) and the inner payload.
    fn json_storage_llvm(
        &self,
        ty: &crate::ast::Type,
    ) -> Option<BasicTypeEnum<'ctx>> {
        use crate::ast::Type;
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        match ty.unlocated() {
            // `Option<T>` is always stored inline as `{disc, payload}` (the
            // discriminant in the low bit of the 8-byte tag at field 0, the payload
            // inline at field 1) — matching the runtime's flattened layout.
            Type::Option(inner) => {
                let p = self.json_storage_llvm(inner)?;
                Some(
                    self.context
                        .struct_type(&[BasicTypeEnum::IntType(i64_ty), p], false)
                        .into(),
                )
            }
            // `Result<T,E>` with a boxable `T`/`E` is stored as
            // `{disc, T, E}` where `T`/`E` themselves keep their own (possibly
            // boxed/flattened) storage — matching the runtime's flattened layout.
            Type::Result(ok, err) => {
                let o = self.json_storage_llvm(ok)?;
                let e = self.json_storage_llvm(err)?;
                Some(
                    self.context
                        .struct_type(
                            &[BasicTypeEnum::IntType(i64_ty), o, e],
                            false,
                        )
                        .into(),
                )
            }
            Type::Name(n, _) if n == "List" => Some(
                self.list_struct_type().into(),
            ),
            Type::Name(n, _) if n == "string" => {
                Some(BasicTypeEnum::IntType(i64_ty))
            }
            // Map/Set/record/tuple/scalars.  Scalar ints narrower than 64 bits
            // are widened to `i64` in the payload slot (matching `llvm_type_for`'s
            // Option/Result widening), so the stored value occupies 8 bytes and
            // field 1 stays at offset 8.
            Type::Name(n, _) => match n.as_str() {
                "i8" | "i16" | "i32" | "u8" | "u16" | "u32" => {
                    Some(BasicTypeEnum::IntType(i64_ty))
                }
                "i64" | "u64" | "char" | "bool" | "f32" | "f64" => {
                    self.llvm_type_for(ty)
                }
                // Map/Set/record/tuple: use the normal inline LLVM type.
                _ => self.llvm_type_for(ty),
            },
            _ => self.llvm_type_for(ty),
        }
    }

    /// The *embedded / boxed* storage layout of `ty` when it appears as a heap-boxed
    /// payload of an enclosing `Option`/`Result`.  A boxable `Option<T>` is stored
    /// reversed as `{value, box_ptr}` (disc becomes `(box_ptr != 0)`); a boxable
    /// `Result<T,E>` keeps `{disc, T, E}` (its fields are inlined by the runtime).
    fn json_storage_llvm_boxed(
        &self,
        ty: &crate::ast::Type,
    ) -> Option<BasicTypeEnum<'ctx>> {
        use crate::ast::Type;
        let i64_ty = self.context.i64_type();
        match ty.unlocated() {
            Type::Option(inner) => {
                // reversed boxed form: {payload_value, box_ptr}
                let v = self.json_storage_llvm(inner)?;
                Some(
                    self.context
                        .struct_type(
                            &[v, BasicTypeEnum::IntType(i64_ty)],
                            false,
                        )
                        .into(),
                )
            }
            Type::Result(ok, err) => self.json_storage_llvm(ty),
            Type::Name(n, _) if n == "List" => Some(self.list_struct_type().into()),
            Type::Name(n, _) if n == "string" => {
                Some(BasicTypeEnum::IntType(i64_ty))
            }
            Type::Name(n, _) => match n.as_str() {
                "i8" | "i16" | "i32" | "u8" | "u16" | "u32" => {
                    Some(BasicTypeEnum::IntType(i64_ty))
                }
                _ => self.llvm_type_for(ty),
            },
            _ => self.llvm_type_for(ty),
        }
    }

    /// Can the recursive generator fully serialize this type (all inner types
    /// handled)? Map/Set/enum return false and fall through to legacy.
    fn json_is_fully_handled(&self, ty: &crate::ast::Type) -> bool {
        let nty = self.json_norm(ty);
        let ty = &nty;
        match ty {
            crate::ast::Type::Option(inner) => self.json_is_fully_handled(inner),
            crate::ast::Type::Result(ok, err) => {
                self.json_is_fully_handled(ok) && self.json_is_fully_handled(err)
            }
            crate::ast::Type::Name(n, args) => match n.as_str() {
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "char"
                | "bool" | "f64" | "f32" | "string" => true,
                "List" => args.len() == 1 && self.json_is_fully_handled(&args[0]),
                "Option" => args.len() == 1 && self.json_is_fully_handled(&args[0]),
                "Result" => {
                    args.len() == 2
                        && self.json_is_fully_handled(&args[0])
                        && self.json_is_fully_handled(&args[1])
                }
                // Set/Map: Phase B work-in-progress. The recursive serializer in
                // `emit_ser_body` now has `Set`/`Map` arms, but the native Set/Map
                // runtime stores elements/values as *handles* (i64), whereas the
                // per-element recursive serializer (`ser_T`) expects the *inline
                // struct* layout (as List does). For scalar elements the handle
                // coincides with the value, but for non-scalar elements (tuples,
                // Option/Result, nested List/Set/Map, records) reading the handle
                // as an inline struct produces malformed IR that crashes the
                // optimizer, and `any_value_to_handle` even discards the Option
                // payload. Until the container element ABI is unified (store
                // elements inline like List, or generate handle-decoding
                // serializers), Set/Map `to_json` must keep using the legacy
                // per-combination tree (the 115 `mimi_*_to_json_*` fns), which
                // already produces L1-correct output. Routed back to legacy here
                // to avoid regressing the `dual_from_json_*` nested suites.
                _ => self.type_defs.get(n).map_or(false, |td| match &td.kind {
                    crate::ast::TypeDefKind::Record(fields) => {
                        fields.iter().all(|f| self.json_is_fully_handled(&f.ty))
                    }
                    // Enums are fully handled by the recursive serializer
                    // (`json_emit_enum`): the `{i32 tag, i64 payload}` layout is
                    // read inline, and each variant's payload is serialized by the
                    // same recursive `ser_T` machinery used for records/tuples.
                    crate::ast::TypeDefKind::Enum(_) => true,
                    _ => false,
                }),
            },
            crate::ast::Type::Tuple(elems) => elems.iter().all(|e| self.json_is_fully_handled(e)),
            _ => false,
        }
    }

    /// Build the LLVM struct type for the `string` type (used for field GEP).
    fn json_string_struct_type(&self) -> Result<inkwell::types::StructType<'ctx>, CompileError> {
        let t = self
            .llvm_type_for(&crate::ast::Type::Name("string".to_string(), vec![]))
            .ok_or_else(|| CompileError::Generic("no llvm type for string".into()))?;
        Ok(t.into_struct_type())
    }

    /// Call a runtime JSON helper that returns a fresh `*mut c_char`.
    fn json_call_rt(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let f = self.get_runtime_fn(name)?;
        let raw = self
            .build_call(f, args, name)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError(format!("{} returned void", name)))?
            .into_pointer_value();
        Ok(raw)
    }

    /// Bitcast a generated serializer FunctionValue into the `JsonSerCb` pointer
    /// type expected by `mimi_json_join_list`.
    fn json_cb_ptr(
        &self,
        ser: inkwell::values::FunctionValue<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let join_fn = self.get_runtime_fn("mimi_json_join_list")?;
        let cb_ty = join_fn.get_type().get_param_types()[1];
        let cb_ty_bte = match cb_ty {
            BasicMetadataTypeEnum::PointerType(p) => BasicTypeEnum::PointerType(p),
            _ => {
                return Err(CompileError::Generic(
                    "json cb param is not a pointer type".into(),
                ))
            }
        };
        let fptr = ser.as_global_value().as_pointer_value();
        let cb = self
            .build_bit_cast(
                BasicValueEnum::PointerValue(fptr),
                cb_ty_bte,
                "json_cb",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        Ok(cb.into_pointer_value())
    }

    /// Generate (or fetch cached) the serializer function for `ty`.
    /// `is_boxed` distinguishes the embedded/heap-boxed layout (`{value,
    /// box_ptr}`, discriminant from `(box_ptr != 0)`) from the standalone layout
    /// (`{disc, payload}`).
    /// A short, layout-describing string for an LLVM type, used to key cached
    /// serializers by their real storage layout (so packed/unpacked variants of
    /// the same Mimi type get distinct functions).
    fn json_type_brief(&self, t: BasicTypeEnum<'ctx>) -> String {
        match t {
            BasicTypeEnum::IntType(i) => format!("i{}", i.get_bit_width()),
            BasicTypeEnum::FloatType(f) => format!("f{}", f.get_bit_width()),
            BasicTypeEnum::PointerType(_) => "p".to_string(),
            BasicTypeEnum::ArrayType(a) => format!("a{}", a.len()),
            BasicTypeEnum::StructType(s) => {
                let parts: Vec<String> = s
                    .get_field_types()
                    .iter()
                    .map(|f| self.json_type_brief(f.clone()))
                    .collect();
                format!("S({})", parts.join(""))
            }
            _ => "x".to_string(),
        }
    }

    fn get_or_emit_json_ser(
        &mut self,
        ty: &crate::ast::Type,
        actual_ty: Option<BasicTypeEnum<'ctx>>,
        is_boxed: bool,
    ) -> Result<inkwell::values::FunctionValue<'ctx>, CompileError> {
        let nty = self.json_norm(ty);
        let ty = &nty;
        let san = Self::json_type_name(ty);
        // Key the cached serializer by the *real* storage layout too: two requests
        // for the same Mimi `ty` but with different `actual_ty` (e.g. packed vs
        // unpacked nested structs) need distinct functions so each one's GEP
        // offsets match its own runtime layout.
        let lay = match actual_ty {
            None => "Ln".to_string(),
            Some(BasicTypeEnum::StructType(st)) => {
                let parts: Vec<String> = st
                    .get_field_types()
                    .iter()
                    .map(|f| self.json_type_brief(f.clone()))
                    .collect();
                format!("Ls{}", parts.join(""))
            }
            Some(other) => format!("Lo{}", self.json_type_brief(other)),
        };
        let fname = format!(
            "mimi_to_json_{}_{}_{}",
            san,
            lay,
            if is_boxed { "b" } else { "s" }
        );
        if let Some(f) = self.module.get_function(&fname) {
            return Ok(f);
        }
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
        let f = self
            .module
            .add_function(&fname, fn_ty, Some(inkwell::module::Linkage::Internal));
        let entry = self.context.append_basic_block(f, "entry");
        let saved = self.builder.get_insert_block();
        self.builder.position_at_end(entry);
        let slot = f.get_first_param().unwrap().into_pointer_value();
        let raw = self.emit_ser_body(ty, slot, actual_ty, is_boxed)?;
        self.builder
            .build_return(Some(&raw))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        if std::env::var_os("MIMI_JSON_VERIFY").is_some() {
            if let Err(msg) = self.module.verify() {
                eprintln!("[JSONVERIFY] ser {} FAILED:\n{}", san, msg.to_string());
            }
        }
        if std::env::var_os("MIMI_JSON_IR").is_some() && san.contains("Option_Name__i64") {
            eprintln!("=== IR for {} ===", san);
            f.print_to_stderr();
        }
        self.builder.position_at_end(saved.unwrap());
        Ok(f)
    }

    /// Build the body of `ser_T(slot)` for type `ty`.
    /// `actual_ty`, when present, is the *real* LLVM storage type of this value
    /// (e.g. the resolved emitter's `lower_type`, which embeds a `List` payload
    /// into `Option<List>` as `{i1,{i64,ptr}}`); it overrides the force-heap
    /// `llvm_type_for` so the serializer's GEP/field layout matches the actual
    /// variable storage. Nested serializers are generated with `None` and rely
    /// on `llvm_type_for` (correct for records/tuples/lists, whose layout is
    /// deterministic).
    fn emit_ser_body(
        &mut self,
        ty: &crate::ast::Type,
        slot: inkwell::values::PointerValue<'ctx>,
        actual_ty: Option<BasicTypeEnum<'ctx>>,
        is_boxed: bool,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let nty = self.json_norm(ty);
        let ty = &nty;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let load_i64 = || {
            self.build_load(BasicTypeEnum::IntType(i64_ty), slot, "json_ld")
                .map(|v| v.into_int_value())
        };
        match ty {
            crate::ast::Type::Name(n, args) => match n.as_str() {
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "char" => {
                    let signed =
                        matches!(n.as_str(), "i8" | "i16" | "i32" | "i64" | "char");
                    let v = self.json_load_int_as_i64(slot, actual_ty, signed)?;
                    self.json_call_rt(
                        "mimi_json_int_to_string",
                        &[BasicMetadataValueEnum::IntValue(v)],
                    )
                }
                "bool" => {
                    let v = self.json_load_int_as_i64(slot, actual_ty, false)?;
                    self.json_call_rt(
                        "mimi_json_bool_to_string",
                        &[BasicMetadataValueEnum::IntValue(v)],
                    )
                }
                "f64" | "f32" => {
                    let bits = self.json_load_float_as_i64(slot, actual_ty)?;
                    self.json_call_rt(
                        "mimi_json_f64_to_string",
                        &[BasicMetadataValueEnum::IntValue(bits)],
                    )
                }
                "string" => {
                    // The slot holds an `i64` whose value is a pointer to the
                    // string storage (basic `{ptr,len}` struct, or a `MimiStr`
                    // fat box for `List<string>`). `mimi_json_string_value`
                    // decodes it (magic-aware) into a heap JSON string.
                    let slot_ptr = self
                        .build_pointer_cast(
                            slot,
                            i8_ptr,
                            "json_s_slot",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.json_call_rt(
                        "mimi_json_string_value",
                        &[BasicMetadataValueEnum::PointerValue(slot_ptr)],
                    )
                }
                "List" => {
                    let inner = &args[0];
                    let v = load_i64()?;
                    let lp = self
                        .build_int_to_ptr(
                            v,
                            self.list_struct_type().ptr_type(inkwell::AddressSpace::default()),
                            "json_lp",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let inner_llvm = self
                        .json_native_scalar_llvm(inner)
                        .or_else(|| self.llvm_type_for(inner));
                    // Scalar/string elements are stored by *value* in the list's
                    // data array (e.g. an `i32` occupies 4 raw bytes, not a widened
                    // `i64`); struct/tuple/container elements are stored as a
                    // *pointer* (8 bytes). Pass the real element storage type as
                    // `actual_ty` so the per-element serializer reads the correct
                    // width, and use that width as the iteration stride. The
                    // native-width LLVM type (not the widened `llvm_type_for`) is
                    // what matches the data array, so narrowed ints/floats get the
                    // correct 1/2/4-byte stride.
                    let elem_size = if self.json_is_scalar_or_string(inner) {
                        inner_llvm
                            .as_ref()
                            .and_then(|t| t.size_of())
                            .and_then(|sz| sz.get_zero_extended_constant())
                            .unwrap_or(8)
                    } else {
                        8
                    };
                    let ser_inner = self.get_or_emit_json_ser(inner, inner_llvm, false)?;
                    let cb = self.json_cb_ptr(ser_inner)?;
                    self.json_call_rt(
                        "mimi_json_join_list",
                        &[
                            BasicMetadataValueEnum::PointerValue(lp),
                            BasicMetadataValueEnum::PointerValue(cb),
                            BasicMetadataValueEnum::IntValue(
                                i64_ty.const_int(elem_size, false),
                            ),
                            BasicMetadataValueEnum::IntValue(
                                i64_ty.const_int(1u64, false),
                            ),
                        ],
                    )
                }
                "Set" => {
                    // `try_emit_json_recursive` stored `ptr_to_int(val_ptr)` (the
                    // *address* of the Set variable storage) in the slot; the Set
                    // variable itself holds the `SetHandle` (an i64), so deref once
                    // to recover the handle. The per-element serializer callback
                    // then receives a `slot` that is a pointer to the i64
                    // element-handle, identical to the `List` element-callback ABI.
                    let inner = &args[0];
                    let v = load_i64()?;
                    let hptr = self
                        .build_int_to_ptr(
                            v,
                            i64_ty.ptr_type(inkwell::AddressSpace::default()),
                            "json_set_hp",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let handle = self
                        .build_load(i64_ty, hptr, "json_set_h")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_int_value();
                    let ser_inner = self.get_or_emit_json_ser(inner, None, false)?;
                    let cb = self.json_cb_ptr(ser_inner)?;
                    self.json_call_rt(
                        "mimi_json_serialize_set",
                        &[
                            BasicMetadataValueEnum::IntValue(handle),
                            BasicMetadataValueEnum::PointerValue(cb),
                        ],
                    )
                }
                "Map" => {
                    // `try_emit_json_recursive` stored `ptr_to_int(val_ptr)` (the
                    // *address* of the Map variable storage) in the slot; the Map
                    // variable itself holds the `MapHandle` (an i64), so deref once
                    // to recover the handle. The per-value serializer callback then
                    // receives a `slot` that is a pointer to the i64 value-handle.
                    let val = &args[1];
                    let v = load_i64()?;
                    let hptr = self
                        .build_int_to_ptr(
                            v,
                            i64_ty.ptr_type(inkwell::AddressSpace::default()),
                            "json_map_hp",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let handle = self
                        .build_load(i64_ty, hptr, "json_map_h")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_int_value();
                    let ser_val = self.get_or_emit_json_ser(val, None, false)?;
                    let cb = self.json_cb_ptr(ser_val)?;
                    self.json_call_rt(
                        "mimi_json_serialize_map",
                        &[
                            BasicMetadataValueEnum::IntValue(handle),
                            BasicMetadataValueEnum::PointerValue(cb),
                        ],
                    )
                }
                _ => {
                    // Record, Enum, or other named struct type.
                    if let Some(td) = self.type_defs.get(n) {
                        match &td.kind {
                            crate::ast::TypeDefKind::Record(fields) => {
                                let struct_ty = self
                                    .llvm_type_for(ty)
                                    .ok_or_else(|| {
                                        CompileError::Generic(format!("no llvm type for {}", n))
                                    })?
                                    .into_struct_type();
                                let v = load_i64()?;
                                let sp = self
                                    .build_int_to_ptr(
                                        v,
                                        struct_ty.ptr_type(inkwell::AddressSpace::default()),
                                        "json_rp",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                // Sort fields by name to match the VM's BTreeMap
                                // ordering; keep the struct declaration index for GEP.
                                let mut sorted: Vec<(String, crate::ast::Type, u32)> = fields
                                    .iter()
                                    .enumerate()
                                    .map(|(i, f)| (f.name.clone(), f.ty.clone(), i as u32))
                                    .collect();
                                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                                let names: Vec<String> =
                                    sorted.iter().map(|(n, _, _)| n.clone()).collect();
                                let field_types: Vec<crate::ast::Type> =
                                    sorted.iter().map(|(_, t, _)| t.clone()).collect();
                                let field_indices: Vec<u32> =
                                    sorted.iter().map(|(_, _, i)| *i).collect();
                                let san = Self::json_type_name(ty);
                                return self.json_emit_join_slots(
                                    struct_ty, &field_types, &field_indices, sp, Some(&names), 1,
                                    &san,
                                );
                            }
                            crate::ast::TypeDefKind::Enum(variants) => {
                                let v = load_i64()?;
                                let vs = variants.to_vec();
                                return self.json_emit_enum(ty, &vs, v);
                            }
                            _ => {}
                        }
                    }
                    Err(CompileError::Generic(format!(
                        "to_json: unsupported named type {}",
                        n
                    )))
                }
            },
            crate::ast::Type::Tuple(elems) => {
                let struct_ty = self
                    .llvm_type_for(ty)
                    .ok_or_else(|| CompileError::Generic("no llvm type for tuple".into()))?
                    .into_struct_type();
                let v = load_i64()?;
                let sp = self
                    .build_int_to_ptr(
                        v,
                        struct_ty.ptr_type(inkwell::AddressSpace::default()),
                        "json_tp",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let field_types: Vec<crate::ast::Type> = elems.iter().cloned().collect();
                let field_indices: Vec<u32> = (0..field_types.len() as u32).collect();
                let san = Self::json_type_name(ty);
                self.json_emit_join_slots(
                    struct_ty,
                    &field_types,
                    &field_indices,
                    sp,
                    None,
                    0,
                    &san,
                )
            }
            crate::ast::Type::Option(inner) => {
                // The runtime always stores `Option<T>` inline as `{disc, payload}`
                // (discriminant at field 0, payload inline at field 1).  When we
                // have the *real* storage type (`actual_ty`, e.g. the resolved
                // emitter's packed struct), use it directly so GEP offsets match
                // the runtime exactly; otherwise fall back to the same inline shape
                // produced by `json_storage_llvm`.
                let struct_ty: inkwell::types::StructType<'ctx> = match actual_ty {
                    Some(BasicTypeEnum::StructType(st)) => st,
                    _ => self
                        .json_storage_llvm(ty)
                        .ok_or_else(|| CompileError::Generic("no llvm type for Option".into()))?
                        .into_struct_type(),
                };
                let v = load_i64()?;
                let sp = self
                    .build_int_to_ptr(
                        v,
                        struct_ty.ptr_type(inkwell::AddressSpace::default()),
                        "json_op",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                // Discriminant = low bit of the 8-byte tag at field 0.
                let disc_ptr = self
                    .gep()
                    .build_struct_gep(struct_ty, sp, 0, "json_o_disc")
                    .map_err(|e| CompileError::LlvmError(format!("{:?}", e)))?;
                let disc_i64 = self
                    .build_load(
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        disc_ptr,
                        "json_o_disc_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let disc_bit = self
                    .builder
                    .build_and(
                        disc_i64,
                        self.context.i64_type().const_int(1u64, false),
                        "json_o_disc_bit",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let tag = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        disc_bit,
                        self.context.i64_type().const_int(0u64, false),
                        "json_o_is_some",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let pl_ptr = self
                    .gep()
                    .build_struct_gep(struct_ty, sp, 1, "json_o_pl")
                    .map_err(|e| CompileError::LlvmError(format!("{:?}", e)))?;
                let inner_actual = struct_ty.get_field_type_at_index(1);
                // A container payload is heap-packed (the field holds a pointer) when
                // the field's *runtime* type is not the inline struct itself — i.e.
                // `actual_ty` field type is a scalar/pointer (`i64`) rather than the
                // aggregate. The legacy emitter force-heaps `Option<Container>` as
                // `{i1, i64}` (field 1 = pointer), while the resolved emitter embeds
                // it as `{i64, {i64, ptr}}` (field 1 = inline struct). Disambiguate
                // from the field type so `load_i64(slot)` reads the right thing.
                let inner_is_boxed =
                    !matches!(inner_actual, Some(BasicTypeEnum::StructType(_)));
                let cur_fn = self
                    .builder
                    .get_insert_block()
                    .unwrap()
                    .get_parent()
                    .unwrap();
                let some_bb = self.context.append_basic_block(cur_fn, "json_o_some");
                let none_bb = self.context.append_basic_block(cur_fn, "json_o_none");
                let merge_bb = self.context.append_basic_block(cur_fn, "json_o_merge");
                self.builder
                    .build_conditional_branch(tag, some_bb, none_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(some_bb);
                let (inner_raw, inner_def_bb) = if !self.json_is_scalar_or_string(inner) {
                    self.json_ser_container_payload_slot(inner, pl_ptr, inner_actual, inner_is_boxed)?
                } else {
                    let inner_field_ty = inner_actual
                        .ok_or_else(|| CompileError::Generic("option payload field type missing".into()))?;
                    self.json_ser_field_call(inner, pl_ptr, inner_field_ty)?
                };
                let some_w = self.json_call_rt(
                    "mimi_json_some",
                    &[BasicMetadataValueEnum::PointerValue(inner_raw)],
                )?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(none_bb);
                let none_w = self.json_call_rt("mimi_json_none", &[])?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let phi = self
                    .builder
                    .build_phi(i8_ptr, "json_o_phi")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                phi.add_incoming(&[(&some_w, inner_def_bb), (&none_w, none_bb)]);
                Ok(phi.as_basic_value().into_pointer_value())
            }
            crate::ast::Type::Result(ok, err) => {
                // Same inline `{disc, ok, err}` storage as `Option` (see above):
                // use the real `actual_ty` when available so GEP offsets match the
                // runtime, and thread the real ok/err field types into the nested
                // serializers.
                let struct_ty: inkwell::types::StructType<'ctx> = match actual_ty {
                    Some(BasicTypeEnum::StructType(st)) => st,
                    _ => self
                        .json_storage_llvm(ty)
                        .ok_or_else(|| CompileError::Generic("no llvm type for Result".into()))?
                        .into_struct_type(),
                };
                let v = load_i64()?;
                let sp = self
                    .build_int_to_ptr(
                        v,
                        struct_ty.ptr_type(inkwell::AddressSpace::default()),
                        "json_rp",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let disc_ptr = self
                    .gep()
                    .build_struct_gep(struct_ty, sp, 0, "json_r_disc")
                    .map_err(|e| CompileError::LlvmError(format!("{:?}", e)))?;
                let disc_i64 = self
                    .build_load(
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        disc_ptr,
                        "json_r_disc_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let disc_bit = self
                    .builder
                    .build_and(
                        disc_i64,
                        self.context.i64_type().const_int(1u64, false),
                        "json_r_disc_bit",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let is_ok = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        disc_bit,
                        self.context.i64_type().const_int(0u64, false),
                        "json_r_is_ok",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let cur_fn = self
                    .builder
                    .get_insert_block()
                    .unwrap()
                    .get_parent()
                    .unwrap();
                let ok_bb = self.context.append_basic_block(cur_fn, "json_r_ok");
                let err_bb = self.context.append_basic_block(cur_fn, "json_r_err");
                let merge_bb = self.context.append_basic_block(cur_fn, "json_r_merge");
                self.builder
                    .build_conditional_branch(is_ok, ok_bb, err_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(ok_bb);
                let ok_ptr = self
                    .gep()
                    .build_struct_gep(struct_ty, sp, 1, "json_r_okp")
                    .map_err(|e| CompileError::LlvmError(format!("{:?}", e)))?;
                let ok_actual = struct_ty.get_field_type_at_index(1);
                let ok_is_boxed = !matches!(ok_actual, Some(BasicTypeEnum::StructType(_)));
                let (ok_raw, ok_def_bb) = if !self.json_is_scalar_or_string(ok) {
                    self.json_ser_container_payload_slot(ok, ok_ptr, ok_actual, ok_is_boxed)?
                } else {
                    let ok_field_ty = ok_actual
                        .ok_or_else(|| CompileError::Generic("result ok field type missing".into()))?;
                    self.json_ser_field_call(ok, ok_ptr, ok_field_ty)?
                };
                let ok_w = self.json_call_rt(
                    "mimi_json_ok",
                    &[BasicMetadataValueEnum::PointerValue(ok_raw)],
                )?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(err_bb);
                let err_ptr = self
                    .gep()
                    .build_struct_gep(struct_ty, sp, 2, "json_r_errp")
                    .map_err(|e| CompileError::LlvmError(format!("{:?}", e)))?;
                let err_actual = struct_ty.get_field_type_at_index(2);
                // Same heap-packed detection as the Option payload: a container
                // `Err` payload is boxed when the field type is not the inline
                // struct itself (legacy force-heap `{i1, i64}` → boxed; resolved
                // embedded struct → not boxed).
                let err_is_boxed = !matches!(err_actual, Some(BasicTypeEnum::StructType(_)));
                let (err_raw, err_def_bb) = if !self.json_is_scalar_or_string(err) {
                    self.json_ser_container_payload_slot(err, err_ptr, err_actual, err_is_boxed)?
                } else {
                    let err_field_ty = err_actual
                        .ok_or_else(|| CompileError::Generic("result err field type missing".into()))?;
                    self.json_ser_field_call(err, err_ptr, err_field_ty)?
                };
                let err_w = self.json_call_rt(
                    "mimi_json_err",
                    &[BasicMetadataValueEnum::PointerValue(err_raw)],
                )?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let phi = self
                    .builder
                    .build_phi(i8_ptr, "json_r_phi")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                phi.add_incoming(&[(&ok_w, ok_def_bb), (&err_w, err_def_bb)]);
                Ok(phi.as_basic_value().into_pointer_value())
            }
            _ => Err(CompileError::Generic(format!(
                "to_json: unsupported type {:?}",
                ty
            ))),
        }
    }

    /// Build the slot pointer passed to an element/field serializer. For
    /// struct-types it is a temp i64 holding the field address; for string/list
    /// it is a temp i64 holding the char/list pointer; for primitives the field
    /// storage itself is the slot.
    fn json_field_slot_ptr(
        &mut self,
        ty: &crate::ast::Type,
        field_ptr: inkwell::values::PointerValue<'ctx>,
        field_llvm_ty: BasicTypeEnum<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let nty = self.json_norm(ty);
        let ty = &nty;
        let i64_ty = self.context.i64_type();
        if self.json_is_struct_type(ty) {
            // `Option<T>` / `Result<T>` payloads are stored two ways, chosen by
            // the program's global heap-packing: embedded inline (the field *is*
            // the payload struct → `ptr_to_int` of the field) or heap-packed
            // (the field holds a pointer → `load i64` of the field gives the
            // `ptrtoint` of the payload). `field_llvm_ty` (the *actual* field
            // type from the parent struct) disambiguates.
            let tmp = self
                .build_alloca(i64_ty, "json_fslot")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let as_i64 = if field_llvm_ty.is_struct_type() {
                self.build_ptr_to_int(field_ptr, i64_ty, "json_fp_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
            } else {
                self.build_load(
                    BasicTypeEnum::IntType(i64_ty),
                    field_ptr,
                    "json_fp_ld",
                )
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                .into_int_value()
            };
            self.build_store(tmp, as_i64)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            Ok(tmp)
        } else if let crate::ast::Type::Name(n, _) = ty {
            match n.as_str() {
                "string" => {
                    let tmp = self
                        .build_alloca(i64_ty, "json_fslot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    // A string *field* may be stored either as a `string` struct by
                    // value (then `field_ptr` is a `string*`, so the slot holds the
                    // struct address) or as an `i64` handle (then `field_ptr` points
                    // at the handle and the slot must hold the handle *value*).
                    // `field_llvm_ty` disambiguates the two layouts.
                    let val = if field_llvm_ty.is_struct_type() {
                        self.build_ptr_to_int(field_ptr, i64_ty, "json_fs_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        self.build_load(
                            BasicTypeEnum::IntType(i64_ty),
                            field_ptr,
                            "json_fs_ld",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_int_value()
                    };
                    self.build_store(tmp, val)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    Ok(tmp)
                }
                "List" | "Set" | "Map" => {
                    // Container fields / Option·Result payloads can be stored two
                    // ways, and the choice is context-dependent (a program that
                    // contains `List<List>` forces global heap-packing):
                    //   * embedded inline: the field *is* the `{data,len}` struct
                    //     (e.g. `Option<List>` as `{i1, {i64, ptr}}`) → slot holds
                    //     the struct address (`ptr_to_int` of the field);
                    //   * heap-packed: the field holds a *pointer* to the struct
                    //     (e.g. `Option<List>` as `{i1, ptr}`) → slot holds the
                    //     pointer (`load i64` of the field).
                    // `field_llvm_ty` is the *actual* field type (from the parent
                    // struct's `llvm_type_for`), so it tells us which case holds.
                    let tmp = self
                        .build_alloca(i64_ty, "json_fslot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let val = if field_llvm_ty.is_struct_type() {
                        self.build_ptr_to_int(field_ptr, i64_ty, "json_fc_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        self.build_load(
                            BasicTypeEnum::IntType(i64_ty),
                            field_ptr,
                            "json_fc_ld",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_int_value()
                    };
                    self.build_store(tmp, val)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    Ok(tmp)
                }
                _ => {
                    // Other nominal fields. Scalar leaf types (i8/i16/i32/i64/
                    // u8/u16/u32/u64/char/bool/f32/f64) are `Type::Name`
                    // variants, so they reach this arm too — for those the field
                    // *holds the value*, so load it (zero-extending/bit-casting
                    // as needed). Only genuinely nested records/enums are stored
                    // by value as a struct the serializer `inttoptr`s from, which
                    // is the `ptr_to_int` fallback below.
                    let tmp = self
                        .build_alloca(i64_ty, "json_fslot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let as_i64 = match field_llvm_ty {
                        BasicTypeEnum::IntType(it) => {
                            let loaded = self
                                .build_load(field_llvm_ty, field_ptr, "json_fv")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            if it.get_bit_width() < 64 {
                                self.builder
                                    .build_int_z_extend(
                                        loaded.into_int_value(),
                                        i64_ty,
                                        "json_fzext",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            } else {
                                loaded.into_int_value()
                            }
                        }
                        BasicTypeEnum::FloatType(_) => {
                            let loaded = self
                                .build_load(field_llvm_ty, field_ptr, "json_fv")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder
                                .build_bit_cast(
                                    loaded,
                                    BasicTypeEnum::IntType(i64_ty),
                                    "json_fbits",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                .into_int_value()
                        }
                        _ => self
                            .build_ptr_to_int(field_ptr, i64_ty, "json_fp_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                    };
                    self.build_store(tmp, as_i64)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    Ok(tmp)
                }
            }
        } else {
            // Scalar field (i64 / f64 / bool / …): the top-level serializer
            // contract is `slot = pointer to an i64 holding the value`, and
            // `mimi_json_join_slots` dereferences `slots[j]` once to obtain that
            // slot. So wrap the loaded scalar in a fresh i64 slot here (mirroring
            // `try_emit_json_recursive`'s scalar handling) instead of returning
            // `field_ptr` directly — otherwise the callback receives the scalar
            // VALUE reinterpreted as a pointer and dereferences garbage, corrupting
            // adjacent stack slots (non-deterministic SIGSEGV on later calls).
            let tmp = self
                .build_alloca(i64_ty, "json_fslot")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let loaded = self
                .build_load(field_llvm_ty, field_ptr, "json_fv")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let as_i64 = match loaded {
                BasicValueEnum::IntValue(iv) => {
                    if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_z_extend(iv, i64_ty, "json_fzext")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    }
                }
                BasicValueEnum::FloatValue(fv) => self
                    .builder
                    .build_bit_cast(
                        BasicValueEnum::FloatValue(fv),
                        BasicTypeEnum::IntType(i64_ty),
                        "json_fbits",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value(),
                _ => loaded.into_int_value(),
            };
            self.build_store(tmp, as_i64)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            Ok(tmp)
        }
    }

    /// Serialize a single field/element of type `ty` whose value lives at
    /// `field_ptr`, returning a fresh `*mut c_char`. `field_llvm_ty` is the
    /// *actual* LLVM type of the field (from the parent struct's layout) and is
    /// used to distinguish inline vs heap-packed container storage.
    fn json_ser_field_call(
        &mut self,
        ty: &crate::ast::Type,
        field_ptr: inkwell::values::PointerValue<'ctx>,
        field_llvm_ty: BasicTypeEnum<'ctx>,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        ),
        CompileError,
    > {
        let slot = self.json_field_slot_ptr(ty, field_ptr, field_llvm_ty)?;
        // `json_field_slot_ptr` returns a pointer of the field's element type;
        // the serializer contract expects a uniform `i8*`, so bitcast.
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let slot = self
            .build_bit_cast(
                BasicValueEnum::PointerValue(slot),
                BasicTypeEnum::PointerType(i8_ptr),
                "json_fser_slot",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let ser = self.get_or_emit_json_ser(ty, Some(field_llvm_ty), false)?;
        let raw = self
            .build_call(ser, &[BasicMetadataValueEnum::PointerValue(slot)], "json_fser")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("json field ser returned void".into()))?
            .into_pointer_value();
        let def_bb = self
            .builder
            .get_insert_block()
            .ok_or_else(|| CompileError::Generic("no insert block for json_ser_field_call".into()))?;
        Ok((raw, def_bb))
    }

    /// Compute the serializer slot for a *container* payload of an
    /// `Option<T>` / `Result<T, E>` whose storage location is `field_ptr`.
    ///
    /// The container payload (`List`/`Map`/`Set`/`Record`/`Tuple`/`Option`/
    /// `Result`) is stored one of two ways depending on the program's global
    /// heap-packing, and the static `actual_ty` we are given does **not**
    /// reliably reflect the runtime layout:
    ///   * embedded inline — the field *is* the payload struct; the serializer
    ///     needs `slot` such that `load_i64(slot) == ptrtoint(&payload_struct)`,
    ///     i.e. `slot` holds `ptrtoint(field_ptr)`;
    ///   * heap-packed — the field *holds a pointer* to the payload struct; the
    ///     serializer needs `load_i64(slot) == ptrtoint(&payload_struct)`, i.e.
    ///     `slot == field_ptr` (so `load_i64(slot)` reads the pointer value).
    ///
    /// We disambiguate at runtime: read the 8 raw bytes at `field_ptr`. For a
    /// heap-packed layout that is a real heap/stack pointer (always a large
    /// address); for an embedded layout it is the first word of the inline
    /// struct (e.g. a `List` length, always a small non-pointer value). A real
    /// pointer exceeds `1_000_000`, so we branch on that.
    fn json_ser_container_payload_slot(
        &mut self,
        ty: &crate::ast::Type,
        field_ptr: inkwell::values::PointerValue<'ctx>,
        actual_ty: Option<BasicTypeEnum<'ctx>>,
        is_boxed: bool,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        ),
        CompileError,
    > {
        // Every serializer's slot contract is `load_i64(slot) ==
        // ptrtoint(&target_struct)`.
        //
        // For an *embedded* container payload (`is_boxed == false`, e.g. the `Ok`
        // payload of a `Result` or the payload of an `Option`) the struct lives
        // inline at `field_ptr`, so `slot` holds `ptrtoint(field_ptr)`.
        //
        // For a *boxed* container payload (`is_boxed == true`, e.g. the `Err`
        // payload of a `Result<_, Container>` where the runtime stores the inner
        // value behind a heap pointer held in the `i64` err field) `field_ptr`
        // points at an `i64` that *is* a pointer to the payload struct; we must
        // load that pointer and use it as the slot value.
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let emb_tmp = self
            .build_alloca(i64_ty, "json_cpay_tmp")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let field_addr = if is_boxed {
            // `field_ptr` addresses an `i64` slot holding the heap pointer.
            self.build_load(
                BasicTypeEnum::IntType(i64_ty),
                field_ptr,
                "json_cpay_box_ld",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value()
        } else {
            self.build_ptr_to_int(field_ptr, i64_ty, "json_cpay_addr")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        };
        self.build_store(emb_tmp, field_addr)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let slot = self
            .build_bit_cast(
                BasicValueEnum::PointerValue(emb_tmp),
                BasicTypeEnum::PointerType(i8_ptr),
                "json_cpay_bc",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        // Serialize the container payload through its own recursive serializer and
        // return the resulting `*mut c_char` (what the Option/Result arm expects).
        // `actual_ty` is the *real* field type (e.g. the nested struct layout), so
        // the nested serializer's GEP offsets match the runtime exactly.
        let ser = self.get_or_emit_json_ser(ty, actual_ty, false)?;
        let raw = self
            .build_call(ser, &[BasicMetadataValueEnum::PointerValue(slot)], "json_cpay_ser")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("json container payload ser void".into()))?
            .into_pointer_value();
        let cur_bb = self
            .builder
            .get_insert_block()
            .ok_or_else(|| CompileError::Generic("no insert block for json_ser_container_payload_slot".into()))?;
        Ok((raw, cur_bb))
    }

    /// Emit a call to `mimi_json_join_slots` over the struct's fields.
    /// `field_indices[j]` is the struct declaration index for `field_types[j]`
    /// (used for GEP); the JSON ordering follows `field_types`/`names` order.
    fn json_emit_join_slots(
        &mut self,
        struct_ty: inkwell::types::StructType<'ctx>,
        field_types: &[crate::ast::Type],
        field_indices: &[u32],
        sp: inkwell::values::PointerValue<'ctx>,
        names: Option<&[String]>,
        is_object: i64,
        san: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let n = field_types.len();
        let arr_ty = i8_ptr.array_type(n as u32);
        let slots_alloca = self
            .build_alloca(arr_ty, "json_slots")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        for (j, ft) in field_types.iter().enumerate() {
            let struct_idx = field_indices[j];
            let field_ptr = self
                .gep()
                .build_struct_gep(struct_ty, sp, struct_idx, "json_f")
                .map_err(|e| CompileError::LlvmError(format!("{:?}", e)))?;
            let field_llvm_ty = struct_ty
                .get_field_type_at_index(struct_idx)
                .ok_or_else(|| {
                    CompileError::Generic(format!("json field {} type missing", struct_idx))
                })?;
            let slot_j = self.json_field_slot_ptr(ft, field_ptr, field_llvm_ty)?;
            let slot_j_i8 = self
                .build_bit_cast(
                    BasicValueEnum::PointerValue(slot_j),
                    BasicTypeEnum::PointerType(i8_ptr),
                    "json_sj",
                )
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                .into_pointer_value();
            let elem_ptr = self
                .gep()
                .build_in_bounds_gep(
                    arr_ty,
                    slots_alloca,
                    &[i64_ty.const_int(0, false), i64_ty.const_int(j as u64, false)],
                    "json_slots_e",
                )
                .map_err(|e| CompileError::LlvmError(format!("{:?}", e)))?;
            self.build_store(elem_ptr, slot_j_i8)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        }
        // Callback function-pointer array (array of i8*).
        let mut cb_ptrs: Vec<inkwell::values::PointerValue<'ctx>> = Vec::with_capacity(n);
        for ft in field_types {
            let ser = self.get_or_emit_json_ser(ft, None, false)?;
            cb_ptrs.push(ser.as_global_value().as_pointer_value());
        }
        let cb_arr_ty = i8_ptr.array_type(n as u32);
        let cb_g = self
            .module
            .add_global(cb_arr_ty, None, &format!("json_cbs_{}", san));
        cb_g.set_initializer(&i8_ptr.const_array(&cb_ptrs));
        let cb_ptr = cb_g.as_pointer_value();
        // Field-name array (null for tuples).
        let names_ptr = match names {
            Some(ns) => {
                let mut name_ptrs: Vec<inkwell::values::PointerValue<'ctx>> = Vec::with_capacity(n);
                for (i, nm) in ns.iter().enumerate() {
                    let gs = self
                        .builder
                        .build_global_string_ptr(nm, &format!("json_nm_{}_{}", san, i))
                        .map_err(|e| CompileError::LlvmError(format!("{}", e)))?;
                    name_ptrs.push(gs.as_pointer_value());
                }
                let name_g = self.module.add_global(
                    i8_ptr.array_type(n as u32),
                    None,
                    &format!("json_names_{}", san),
                );
                name_g.set_initializer(&i8_ptr.const_array(&name_ptrs));
                name_g.as_pointer_value()
            }
            None => i8_ptr.const_null(),
        };
        let n_val = i64_ty.const_int(n as u64, false);
        let is_obj_val = i64_ty.const_int(is_object as u64, false);
        self.json_call_rt(
            "mimi_json_join_slots",
            &[
                BasicMetadataValueEnum::PointerValue(slots_alloca),
                BasicMetadataValueEnum::PointerValue(cb_ptr),
                BasicMetadataValueEnum::PointerValue(names_ptr),
                BasicMetadataValueEnum::IntValue(n_val),
                BasicMetadataValueEnum::IntValue(is_obj_val),
            ],
        )
    }

    /// Serialize a custom `enum` value (layout `{i32 tag, i64 payload}`, matching
    /// `build_nominal_variant`) by branching on the tag and serializing the
    /// active variant's payload with the same recursive `ser_T` machinery used
    /// for records/tuples. Mirrors the VM's `value_to_json` for enums:
    ///   * nullary variant -> `"TagName"`
    ///   * single-field tuple payload -> `{"TagName":[<elem>]}`
    ///   * multi-field tuple payload -> `{"TagName":[<e0>,<e1>,...]}`
    ///   * record payload -> `{"TagName":{<f0>:<v0>,...}}`
    fn json_emit_enum(
        &mut self,
        ty: &crate::ast::Type,
        variants: &[crate::ast::Variant],
        v: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        // Enum layout: { i32 tag, i64 payload } (matches build_nominal_variant).
        let enum_struct_ty = self
            .context
            .struct_type(&[i32_ty.into(), i64_ty.into()], false);
        let enum_ptr = self
            .build_int_to_ptr(
                v,
                enum_struct_ty.ptr_type(inkwell::AddressSpace::default()),
                "json_enum_p",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let tag_gep = self
            .gep()
            .build_struct_gep(enum_struct_ty, enum_ptr, 0, "json_enum_tag")
            .map_err(|e| CompileError::LlvmError(format!("enum tag gep: {}", e)))?;
        let tag = self
            .build_load(BasicTypeEnum::IntType(i32_ty), tag_gep, "json_enum_tag_v")?
            .into_int_value();
        let pay_gep = self
            .gep()
            .build_struct_gep(enum_struct_ty, enum_ptr, 1, "json_enum_pay")
            .map_err(|e| CompileError::LlvmError(format!("enum pay gep: {}", e)))?;
        let payload = self
            .build_load(BasicTypeEnum::IntType(i64_ty), pay_gep, "json_enum_pay_v")?
            .into_int_value();

        // Tag value == position of the variant when sorted by name
        // (build_nominal_variant assigns tags this way).
        let mut sorted: Vec<&crate::ast::Variant> = variants.iter().collect();
        sorted.sort_by_key(|vv| &vv.name);

        let function = self
            .current_function()
            .ok_or_else(|| CompileError::Generic("json_emit_enum: no current function".into()))?;
        let res_alloca = self
            .build_alloca(i8_ptr, "json_enum_res")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let exit_bb = self.context.append_basic_block(function, "json_enum_exit");
        let default_bb = self.context.append_basic_block(function, "json_enum_def");
        let mut switch_cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        let mut case_bbs: Vec<(usize, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();
        for (i, _variant) in sorted.iter().enumerate() {
            let bb = self
                .context
                .append_basic_block(function, &format!("json_enum_v{}", i));
            switch_cases.push((i32_ty.const_int(i as u64, false), bb));
            case_bbs.push((i, bb));
        }
        self.builder
            .build_switch(tag, default_bb, &switch_cases)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        for (i, bb) in case_bbs {
            self.builder.position_at_end(bb);
            let variant = sorted[i];
            let result = self.json_enum_variant_result(ty, variant, payload)?;
            self.build_store(res_alloca, result)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            self.build_br(exit_bb)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        }
        // Default: unknown tag (should not happen for a well-formed enum).
        self.builder.position_at_end(default_bb);
        let dflt = self.json_call_rt(
            "mimi_json_alloc_literal",
            &[BasicMetadataValueEnum::PointerValue(
                self.builder
                    .build_global_string_ptr("null", "json_enum_deflit")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .as_pointer_value(),
            )],
        )?;
        self.build_store(res_alloca, dflt)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_br(exit_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(exit_bb);
        let result = self
            .build_load(BasicTypeEnum::PointerType(i8_ptr), res_alloca, "json_enum_res_v")?
            .into_pointer_value();
        Ok(result)
    }

    /// Serialize a single enum variant's payload and wrap it as `"TagName"` or
    /// `{"TagName":<payload>}`. `payload` is the loaded `i64` payload field:
    /// for a single scalar field it holds the value (bit-cast / sign-extended);
    /// for a single struct field, or for any multi-field/record payload, it
    /// holds `ptrtoint` of the inline payload struct.
    fn json_enum_variant_result(
        &mut self,
        ty: &crate::ast::Type,
        variant: &crate::ast::Variant,
        payload: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let name_g = self
            .builder
            .build_global_string_ptr(&variant.name, "json_enum_name")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .as_pointer_value();
        let null_ptr = i8_ptr.const_null();
        match &variant.payload {
            None => self.json_call_rt(
                "mimi_json_serialize_enum_variant",
                &[
                    BasicMetadataValueEnum::PointerValue(name_g),
                    BasicMetadataValueEnum::PointerValue(null_ptr),
                ],
            ),
            Some(crate::ast::VariantPayload::Tuple(types)) => {
                if types.len() == 1 {
                    // Single field: serialize the payload i64 directly via ser_T,
                    // then bracket it as `[<elem>]`.
                    let pay_slot = self
                        .build_alloca(i64_ty, "json_enum_payslot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_store(pay_slot, payload)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let pay_i8 = self
                        .build_bit_cast(
                            BasicValueEnum::PointerValue(pay_slot),
                            BasicTypeEnum::PointerType(i8_ptr),
                            "json_enum_pay_i8",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_pointer_value();
                    let ser = self.get_or_emit_json_ser(&types[0], None, false)?;
                    let frag = self
                        .build_call(ser, &[BasicMetadataValueEnum::PointerValue(pay_i8)], "json_enum_frag")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| CompileError::LlvmError("enum ser returned void".into()))?
                        .into_pointer_value();
                    let arr = self.json_call_rt(
                        "mimi_json_surround_brackets",
                        &[BasicMetadataValueEnum::PointerValue(frag)],
                    )?;
                    self.json_call_rt(
                        "mimi_json_serialize_enum_variant",
                        &[
                            BasicMetadataValueEnum::PointerValue(name_g),
                            BasicMetadataValueEnum::PointerValue(arr),
                        ],
                    )
                } else {
                    // Multi-field tuple payload: the i64 holds ptrtoint of a
                    // packed struct `{T0,T1,...}`; serialize it as a JSON array.
                    let field_llvm: Vec<BasicTypeEnum<'ctx>> = types
                        .iter()
                        .map(|t| {
                            self.llvm_type_for(t).ok_or_else(|| {
                                CompileError::Generic(format!("no llvm type for enum field {}", Self::json_type_name(t)))
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let struct_ty = self
                        .context
                        .struct_type(&field_llvm, false);
                    let pay_ptr = self
                        .build_int_to_ptr(
                            payload,
                            struct_ty.ptr_type(inkwell::AddressSpace::default()),
                            "json_enum_mf_p",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let field_types: Vec<crate::ast::Type> = types.iter().cloned().collect();
                    let field_indices: Vec<u32> = (0..field_types.len() as u32).collect();
                    let san = Self::json_type_name(ty);
                    let inner = self.json_emit_join_slots(
                        struct_ty,
                        &field_types,
                        &field_indices,
                        pay_ptr,
                        None,
                        0,
                        &san,
                    )?;
                    self.json_call_rt(
                        "mimi_json_serialize_enum_variant",
                        &[
                            BasicMetadataValueEnum::PointerValue(name_g),
                            BasicMetadataValueEnum::PointerValue(inner),
                        ],
                    )
                }
            }
            Some(crate::ast::VariantPayload::Record(fields)) => {
                // Record payload: the VM serializes enum payloads *positionally*
                // as a JSON array `[f0,f1,...]` (it ignores the record field
                // names for enum payloads), so use the array form here too.
                let field_llvm: Vec<BasicTypeEnum<'ctx>> = fields
                    .iter()
                    .map(|f| {
                        self.llvm_type_for(&f.ty).ok_or_else(|| {
                            CompileError::Generic(format!("no llvm type for enum field {}", f.name))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let struct_ty = self.context.struct_type(&field_llvm, false);
                let pay_ptr = self
                    .build_int_to_ptr(
                        payload,
                        struct_ty.ptr_type(inkwell::AddressSpace::default()),
                        "json_enum_rec_p",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let field_types: Vec<crate::ast::Type> =
                    fields.iter().map(|f| f.ty.clone()).collect();
                let field_indices: Vec<u32> = (0..field_types.len() as u32).collect();
                let san = Self::json_type_name(ty);
                let inner = self.json_emit_join_slots(
                    struct_ty,
                    &field_types,
                    &field_indices,
                    pay_ptr,
                    None,
                    0,
                    &san,
                )?;
                self.json_call_rt(
                    "mimi_json_serialize_enum_variant",
                    &[
                        BasicMetadataValueEnum::PointerValue(name_g),
                        BasicMetadataValueEnum::PointerValue(inner),
                    ],
                )
            }
        }
    }

    /// Top-level entry: if `obj_type` is fully handled by the recursive
    /// generator, build the slot and call the serializer; otherwise return
    /// `None` so the caller falls through to the legacy dispatch tree.
    fn try_emit_json_recursive(
        &mut self,
        obj_type: &str,
        arg0: &BasicMetadataValueEnum<'ctx>,
        actual_ty: Option<BasicTypeEnum<'ctx>>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        let ty = match crate::codegen::expr::call::helpers::parse_type_str(obj_type) {
            Some(t) => t,
            None => return Ok(None),
        };
        if !self.json_is_fully_handled(&ty) {
            return Ok(None);
        }
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // Materialize the value into a pointer we can take an address of.
        let val_ptr: inkwell::values::PointerValue<'ctx> = match arg0 {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::IntValue(iv) => {
                let a = self
                    .build_alloca(i64_ty, "json_vp")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_store(a, *iv)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                a
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                let a = self
                    .build_alloca(i64_ty, "json_vp")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let bits = self
                    .build_bit_cast(
                        BasicValueEnum::FloatValue(*fv),
                        BasicTypeEnum::IntType(i64_ty),
                        "json_bits",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                self.build_store(a, bits)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                a
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                let a = self
                    .build_alloca(sv.get_type(), "json_vp")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_store(a, *sv)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                a
            }
            _ => {
                return Err(CompileError::Generic(format!(
                    "to_json: unsupported arg kind for {}",
                    obj_type
                )))
            }
        };
        // Build the i64 slot consumed by the serializer.
        let slot = self
            .build_alloca(i64_ty, "json_slot")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        if self.json_is_struct_type(&ty) {
            let as_i64 = self
                .build_ptr_to_int(val_ptr, i64_ty, "json_vp_i64")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            self.build_store(slot, as_i64)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        } else {
            match arg0 {
                BasicMetadataValueEnum::IntValue(_) | BasicMetadataValueEnum::FloatValue(_) => {
                    let v = self
                        .build_load(BasicTypeEnum::IntType(i64_ty), val_ptr, "json_ld")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_int_value();
                    self.build_store(slot, v)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                }
                BasicMetadataValueEnum::StructValue(_) => {
                    if let crate::ast::Type::Name(n, _) = &ty {
                        if n == "string" {
                            // `val_ptr` points to the `string` struct by value;
                            // store its address as the `string*` slot.
                            let sp_i64 = self
                                .build_ptr_to_int(val_ptr, i64_ty, "json_s_i64")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_store(slot, sp_i64)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        } else if n == "List" || n == "Set" || n == "Map" {
                            let p_i64 = self
                                .build_ptr_to_int(val_ptr, i64_ty, "json_c_i64")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_store(slot, p_i64)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        }
                    }
                }
                BasicMetadataValueEnum::PointerValue(_) => {
                    if let crate::ast::Type::Name(n, _) = &ty {
                        if n == "List" || n == "Set" || n == "Map" {
                            let p_i64 = self
                                .build_ptr_to_int(val_ptr, i64_ty, "json_c_i64")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_store(slot, p_i64)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        }
                    }
                }
                _ => {}
            }
        }
        let ser = self.get_or_emit_json_ser(&ty, actual_ty, false)?;
        // `slot` is an `i64` alloca holding the value/pointer; the serializer
        // contract expects a uniform `i8*`, so bitcast.
        let slot_i8 = self
            .build_bit_cast(
                BasicValueEnum::PointerValue(slot),
                BasicTypeEnum::PointerType(i8_ptr),
                "json_slot_i8",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let raw = self
            .build_call(
                ser,
                &[BasicMetadataValueEnum::PointerValue(slot_i8)],
                "json_ser",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("json ser returned void".into()))?
            .into_pointer_value();
        self.register_heap_alloc(raw);
        let wrapped = self.wrap_c_string(raw)?;
        Ok(Some(wrapped))
    }

}

/// Convert a BasicValueEnum to its metadata type for indirect calls.
fn basic_value_to_metadata_type<'ctx>(
    val: &BasicValueEnum<'ctx>,
) -> Result<BasicMetadataTypeEnum<'ctx>, CompileError> {
    Ok(match val {
        BasicValueEnum::IntValue(iv) => BasicMetadataTypeEnum::IntType(iv.get_type()),
        BasicValueEnum::FloatValue(fv) => BasicMetadataTypeEnum::FloatType(fv.get_type()),
        BasicValueEnum::PointerValue(pv) => BasicMetadataTypeEnum::PointerType(pv.get_type()),
        BasicValueEnum::StructValue(sv) => BasicMetadataTypeEnum::StructType(sv.get_type()),
        BasicValueEnum::ArrayValue(av) => BasicMetadataTypeEnum::ArrayType(av.get_type()),
        BasicValueEnum::VectorValue(vv) => BasicMetadataTypeEnum::VectorType(vv.get_type()),
        BasicValueEnum::ScalableVectorValue(_) => {
            return Err(CompileError::Generic(
                "scalable vector not supported in Mimi codegen".to_string(),
            ));
        }
    })
}
fn fn_type_for_basic_type<'ctx>(
    ret_ty: BasicTypeEnum<'ctx>,
    params: &[BasicMetadataTypeEnum<'ctx>],
) -> Result<inkwell::types::FunctionType<'ctx>, CompileError> {
    match ret_ty {
        BasicTypeEnum::IntType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::FloatType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::PointerType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::StructType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::ArrayType(t) => Ok(t.fn_type(params, false)),
        _ => Err(CompileError::Generic(
            "closure wrapper: unsupported return type".to_string(),
        )),
    }
}
