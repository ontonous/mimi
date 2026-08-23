#![allow(clippy::unwrap_used)]
use super::super::CallSiteValueExt;
use super::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
use inkwell::IntPredicate;

/// Wave-1 audit fix (devdocs/full-audit-2026-08-05.md §8): one piece of a
/// sized string assembly. The old display emitters concat'd pieces into
/// fixed-size buffers (4096/8192/1024) with `strcat`/`snprintf("%s", …)` and
/// no capacity tracking — silent truncation or heap overflow for long
/// renderings. Sized assembly computes the exact total first, mallocs once,
/// then memcpys each piece at a running offset.
pub(in crate::codegen) enum CatPart<'ctx> {
    /// Compile-time literal fragment (byte length known at emit time).
    Lit(&'static str),
    /// Compile-time fragment with a runtime-built name (e.g. custom enum
    /// `Variant()` labels). Materialized as a global string at emit time.
    Owned(String),
    /// Runtime NUL-terminated C string (length taken with strlen at runtime).
    /// NOTE (0.34.36 probe): legacy display pointers must ALWAYS go through
    /// this variant — string len slots are not reliably maintained on every
    /// legacy path, while runtime string allocations are NUL-terminated.
    Dyn(inkwell::values::PointerValue<'ctx>),
}

/// Wave-1 audit fix (§8, FIX: list display i32 fallback): element kind of a
/// scalar list rendered through the typed display path.
#[derive(Clone, Copy)]
enum ScalarListKind {
    F64,
    I64,
    Bool,
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Q2 (rc-quality-gate-0.34.25b): malloc a display-formatter scratch
    /// buffer and register it for release at the consuming print call.
    /// Display emitters (Result/List/Tuple/Record/Enum/Option/Map formatters)
    /// must use this instead of raw `malloc_or_abort`: the returned buffer
    /// is consumed by exactly one printf/puts and freed by
    /// `flush_display_frees` immediately after that call — no more 256B-
    /// per-printed-Result linear leaks.
    ///
    /// NOT for I/O buffers (input_line / read / exec argv), whose lifetime
    /// is managed by the runtime and must NOT be freed here.
    pub(super) fn malloc_display_buf(
        &self,
        size: inkwell::values::IntValue<'ctx>,
        name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let ptr = self.malloc_or_abort(size, name)?;
        self.register_display_alloc(ptr);
        Ok(ptr)
    }

    /// Wave-1 audit fix (§8, FIX: eprintln wrote stdout): get-or-declare the
    /// libc stream global (`stdin`/`stderr` — glibc exports both as `FILE*`
    /// symbols) and load the stream pointer. Same pattern the legacy
    /// `compile_input` used for `stdin`.
    fn get_stream_global(&self, name: &str) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let global = match self.module.get_global(name) {
            Some(g) => g,
            None => self.module.add_global(i8_ptr_ty, None, name),
        };
        global.set_linkage(inkwell::module::Linkage::External);
        let stream = self
            .builder
            .build_load(
                BasicTypeEnum::PointerType(i8_ptr_ty),
                global.as_pointer_value(),
                name,
            )
            .map_err(|e| CompileError::LlvmError(format!("load {} error: {}", name, e)))?
            .into_pointer_value();
        Ok(stream)
    }

    /// Wave-1 audit fix (§8, FIX: fixed-buffer strcat display emitters):
    /// exact-size string assembly over a compile-time list of parts.
    /// Pass 1 computes `total = 1 (NUL) + Σ len(part)` (strlen for `Dyn`
    /// parts), pass 2 mallocs exactly `total` bytes and memcpys each part at
    /// a running offset, then NUL-terminates. No fixed capacity, no strcat,
    /// no truncation, no overflow.
    ///
    /// When `register_display` is true the result buffer is registered via
    /// `malloc_display_buf` and released by the consuming print's
    /// `flush_display_frees`; JSON producers pass `false` and keep the
    /// existing lifetime discipline of their call sites.
    pub(in crate::codegen) fn sized_cat_parts(
        &self,
        parts: &[CatPart<'ctx>],
        name: &str,
        register_display: bool,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let memcpy_fn = self.get_runtime_fn("memcpy")?;

        // Pass 1: total = 1 (trailing NUL) + Σ part lengths.
        let total_alloca = self.build_alloca(
            BasicTypeEnum::IntType(i64_ty),
            &format!("{}_asz_total", name),
        )?;
        self.build_store(total_alloca, i64_ty.const_int(1, false))?;
        for (pi, part) in parts.iter().enumerate() {
            let add_len: inkwell::values::IntValue<'ctx> = match part {
                CatPart::Lit(s) => i64_ty.const_int(s.len() as u64, false),
                CatPart::Owned(s) => i64_ty.const_int(s.len() as u64, false),
                CatPart::Dyn(p) => self
                    .build_call(
                        strlen_fn,
                        &[BasicMetadataValueEnum::PointerValue(*p)],
                        &format!("{}_asz_len{}", name, pi),
                    )?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
                    .into_int_value(),
            };
            let cur = self
                .build_load(i64_ty, total_alloca, &format!("{}_asz_t{}", name, pi))?
                .into_int_value();
            // M8 (0.35.37): checked add — a wrapped `total` (negative or tiny)
            // would make malloc allocate a small buffer and the trailing
            // NUL write (buf[offset], offset = total-1) go out of bounds.
            // Same SD-7 sadd.with.overflow pattern as list/hof.rs.
            let saddle_ty = self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::IntType(self.context.bool_type()),
                ],
                false,
            );
            let saddle_fn = self
                .module
                .get_function("llvm.sadd.with.overflow.i64")
                .unwrap_or_else(|| {
                    self.module.add_function(
                        "llvm.sadd.with.overflow.i64",
                        saddle_ty.fn_type(
                            &[
                                BasicMetadataTypeEnum::IntType(i64_ty),
                                BasicMetadataTypeEnum::IntType(i64_ty),
                            ],
                            false,
                        ),
                        Some(inkwell::module::Linkage::External),
                    )
                });
            let saddle_call = self
                .builder
                .build_call(
                    saddle_fn,
                    &[
                        BasicMetadataValueEnum::IntValue(cur),
                        BasicMetadataValueEnum::IntValue(add_len),
                    ],
                    &format!("{}_asz_sadd{}", name, pi),
                )
                .map_err(|e| CompileError::LlvmError(format!("sadd.with.overflow: {}", e)))?;
            let saddle_val = saddle_call
                .try_as_basic_value_opt()
                .ok_or_else(|| CompileError::LlvmError("sadd.with.overflow returned void".into()))?
                .into_struct_value();
            let next = self
                .builder
                .build_extract_value(saddle_val, 0, &format!("{}_asz_sum{}", name, pi))
                .map_err(|e| CompileError::LlvmError(format!("extract: {}", e)))?
                .into_int_value();
            let overflow = self
                .builder
                .build_extract_value(saddle_val, 1, &format!("{}_asz_ovf{}", name, pi))
                .map_err(|e| CompileError::LlvmError(format!("extract: {}", e)))?
                .into_int_value();
            let current_fn = self.current_function().ok_or_else(|| {
                CompileError::LlvmError("no current function for sized_cat_parts overflow".into())
            })?;
            let sum_ok_bb = self
                .context
                .append_basic_block(current_fn, &format!("{}_asz_ok{}", name, pi));
            let sum_ovf_bb = self
                .context
                .append_basic_block(current_fn, &format!("{}_asz_ovf_trap{}", name, pi));
            self.builder
                .build_conditional_branch(overflow, sum_ovf_bb, sum_ok_bb)
                .map_err(|e| CompileError::LlvmError(format!("cond_br: {}", e)))?;
            self.builder.position_at_end(sum_ovf_bb);
            let ovf_msg = self
                .builder
                .build_global_string_ptr(
                    "display buffer size overflow",
                    &format!("{}_asz_ovf_msg{}", name, pi),
                )
                .map_err(|e| CompileError::LlvmError(format!("global string: {}", e)))?;
            let abort_fn = self.get_or_declare_abort_fn();
            self.builder
                .build_call(
                    abort_fn,
                    &[BasicMetadataValueEnum::PointerValue(
                        ovf_msg.as_pointer_value(),
                    )],
                    &format!("{}_asz_ovf_abort{}", name, pi),
                )
                .map_err(|e| CompileError::LlvmError(format!("call: {}", e)))?;
            // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
            self.builder
                .build_unreachable()
                .map_err(|e| CompileError::LlvmError(format!("unreachable: {}", e)))?;
            self.builder.position_at_end(sum_ok_bb);
            self.build_store(total_alloca, next)?;
        }
        let total = self
            .build_load(i64_ty, total_alloca, &format!("{}_asz_total_ld", name))?
            .into_int_value();
        let buf = if register_display {
            self.malloc_display_buf(total, &format!("{}_buf", name))?
        } else {
            self.malloc_or_abort(total, &format!("{}_buf", name))?
        };

        // Pass 2: copy each part at the running offset, then NUL-terminate.
        let off_alloca =
            self.build_alloca(BasicTypeEnum::IntType(i64_ty), &format!("{}_asz_off", name))?;
        self.build_store(off_alloca, i64_ty.const_int(0, false))?;
        for (pi, part) in parts.iter().enumerate() {
            let (src, copy_len) = match part {
                CatPart::Lit(s) => {
                    if s.is_empty() {
                        continue;
                    }
                    let g = self
                        .builder
                        .build_global_string_ptr(s, &format!("{}_asz_lit{}", name, pi))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    (
                        g.as_pointer_value(),
                        i64_ty.const_int(s.len() as u64, false),
                    )
                }
                CatPart::Owned(s) => {
                    if s.is_empty() {
                        continue;
                    }
                    let g = self
                        .builder
                        .build_global_string_ptr(s.as_str(), &format!("{}_asz_own{}", name, pi))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    (
                        g.as_pointer_value(),
                        i64_ty.const_int(s.len() as u64, false),
                    )
                }
                CatPart::Dyn(p) => {
                    let l = self
                        .build_call(
                            strlen_fn,
                            &[BasicMetadataValueEnum::PointerValue(*p)],
                            &format!("{}_asz_len2_{}", name, pi),
                        )?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
                        .into_int_value();
                    (*p, l)
                }
            };
            let off = self
                .build_load(i64_ty, off_alloca, &format!("{}_asz_o{}", name, pi))?
                .into_int_value();
            // SAFETY: byte-offset GEP into `buf` (total bytes allocated above);
            // off + copy_len <= total by construction of pass 1.
            let dst = self
                .gep()
                .build_gep(i8_ty, buf, &[off], &format!("{}_asz_dst{}", name, pi))
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            self.build_call(
                memcpy_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(dst),
                    BasicMetadataValueEnum::PointerValue(src),
                    BasicMetadataValueEnum::IntValue(copy_len),
                ],
                &format!("{}_asz_mcpy{}", name, pi),
            )?;
            let off2 = self
                .builder
                .build_int_add(off, copy_len, &format!("{}_asz_o2_{}", name, pi))
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            self.build_store(off_alloca, off2)?;
        }
        let off_end = self
            .build_load(i64_ty, off_alloca, &format!("{}_asz_oend", name))?
            .into_int_value();
        // NUL-terminate via an opaque runtime helper. The earlier direct
        // `store i8 0, buf[oend]` was folded by LLVM O1 together with the
        // straight-line memcpy chain (record display, 0.34.36): the folded
        // offset picked up a stale runtime length and truncated the result
        // to its first byte. A call the optimizer cannot see through breaks
        // the folding.
        // M8 (0.35.37): pass `total` as alloc_size so the runtime aborts
        // (instead of writing out of bounds) if off_end ever exceeds the
        // allocation.
        let nul_fn_ty = self.context.void_type().fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64_ty),
                BasicMetadataTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let nul_fn = self
            .module
            .get_function("mimi_runtime_buf_nul_terminate")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_runtime_buf_nul_terminate",
                    nul_fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        self.build_call(
            nul_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::IntValue(off_end),
                BasicMetadataValueEnum::IntValue(total),
            ],
            &format!("{}_asz_term", name),
        )?;
        Ok(buf)
    }

    /// §8-#96/D-4 residue (0.34.36): wrap a rendered payload as
    /// `Prefix(inner)` with exact-size assembly — replaces the fixed
    /// snprintf("Prefix(%s)") buffers (256B Result / 512B Option / 128B
    /// enum) that silently truncated payloads longer than the buffer.
    /// Returns an UNREGISTERED buffer: the caller stores it in a branch
    /// merge slot and registers the merge-loaded value (defined on every
    /// runtime path) so the consuming print's flush frees it exactly once.
    fn emit_display_wrap(
        &self,
        prefix_lit: &'static str,
        inner: inkwell::values::PointerValue<'ctx>,
        name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.sized_cat_parts(
            &[
                CatPart::Lit(prefix_lit),
                CatPart::Dyn(inner),
                CatPart::Lit(")"),
            ],
            name,
            false,
        )
    }

    /// §8-#96/D-4 residue: same as `emit_display_wrap` but with a runtime
    /// prefix string (custom enum variant names: `Variant(`).
    fn emit_display_wrap_dyn(
        &self,
        prefix_ptr: inkwell::values::PointerValue<'ctx>,
        inner: inkwell::values::PointerValue<'ctx>,
        name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.sized_cat_parts(
            &[
                CatPart::Dyn(prefix_ptr),
                CatPart::Dyn(inner),
                CatPart::Lit(")"),
            ],
            name,
            false,
        )
    }

    /// §8-#96/D-4 residue: heap-copy a static literal into an UNREGISTERED
    /// display buffer. Branch merge slots register the merge-loaded value
    /// unconditionally, so literal-only arms (None()/Enum(?)/Variant()/
    /// Ok(?)) must still produce a malloc'd buffer — returning the global
    /// directly would free(read-only global) at print-flush.
    fn emit_display_lit_copy(
        &self,
        lit: &'static str,
        name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.sized_cat_parts(&[CatPart::Lit(lit)], name, false)
    }

    /// §8-#96/D-4 residue: same as `emit_display_lit_copy` for runtime-built
    /// labels (custom enum `Variant()` names).
    fn emit_display_owned_copy(
        &self,
        lit: String,
        name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.sized_cat_parts(&[CatPart::Owned(lit)], name, false)
    }

    /// §8-#96/D-4 residue: render an i64 payload to a heap C string via
    /// `mimi_to_string_i64` and display-register it (scratch, freed by the
    /// enclosing arm's `flush_display_since` after the wrap consumes it).
    fn emit_display_i64_str(
        &self,
        iv: inkwell::values::IntValue<'ctx>,
        name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let to_i64_fn = self.get_runtime_fn("mimi_to_string_i64")?;
        let s = self
            .build_call(
                to_i64_fn,
                &[BasicMetadataValueEnum::IntValue(iv)],
                &format!("{}_str", name),
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_to_string_i64 returned void".into()))?
            .into_pointer_value();
        self.register_display_alloc(s);
        Ok(s)
    }

    /// §8-#96/D-4 residue: render an f64 payload via `mimi_to_string_f64`
    /// (Rust shortest round-trip Display — matches the VM and the scalar
    /// print path; the old `%g` arms printed only 6 significant digits)
    /// and display-register it as arm scratch.
    fn emit_display_f64_str(
        &self,
        fv: inkwell::values::FloatValue<'ctx>,
        name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let to_f64_fn = self.get_runtime_fn("mimi_to_string_f64")?;
        let s = self
            .build_call(
                to_f64_fn,
                &[BasicMetadataValueEnum::FloatValue(fv)],
                &format!("{}_str", name),
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_to_string_f64 returned void".into()))?
            .into_pointer_value();
        self.register_display_alloc(s);
        Ok(s)
    }

    /// Wave-1 audit fix (§8, FIX: fixed-buffer strcat list display emitters):
    /// two-pass exact-size assembly of `"[", e0, ", ", e1, …, "]"` where each
    /// element piece is rendered by `render_elem(idx)` (called once per
    /// element per pass — element renderers are pure display formatters).
    ///
    /// Pass 1 measures: `total = 3 ("[", "]", NUL) + Σ strlen(piece) + 2·(len-1)`
    /// (pieces freed per iteration via `flush_display_since`).
    /// Pass 2 allocates exactly `total`, re-renders each piece, memcpys it at
    /// the running offset and frees it. The final buffer is registered as a
    /// display buffer (freed by the consuming print).
    fn emit_sized_list_of_pieces<F>(
        &self,
        len: inkwell::values::IntValue<'ctx>,
        sep: &'static str,
        render_elem: F,
        name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>>
    where
        F: Fn(inkwell::values::IntValue<'ctx>) -> MimiResult<inkwell::values::PointerValue<'ctx>>,
    {
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let memcpy_fn = self.get_runtime_fn("memcpy")?;
        let sep_len = sep.len() as u64;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;

        // ---- Pass 1: measure total length ----
        let total_alloca = self.build_alloca(
            BasicTypeEnum::IntType(i64_ty),
            &format!("{}_sz_total", name),
        )?;
        // "[" + "]" + trailing NUL.
        self.build_store(total_alloca, i64_ty.const_int(3, false))?;
        let idx_alloca =
            self.build_alloca(BasicTypeEnum::IntType(i64_ty), &format!("{}_sz_i1", name))?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        let loop1_bb = self
            .context
            .append_basic_block(parent, &format!("{}_sz_loop1", name));
        let body1_bb = self
            .context
            .append_basic_block(parent, &format!("{}_sz_body1", name));
        let done1_bb = self
            .context
            .append_basic_block(parent, &format!("{}_sz_done1", name));
        self.build_br(loop1_bb)?;
        self.builder.position_at_end(loop1_bb);
        let idx1 = self
            .build_load(i64_ty, idx_alloca, &format!("{}_sz_idx1", name))?
            .into_int_value();
        let cont1 = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx1, len, &format!("{}_sz_cont1", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_cond_br(cont1, body1_bb, done1_bb)?;
        self.builder.position_at_end(body1_bb);
        let marker1 = self.display_marker();
        let piece1 = render_elem(idx1)?;
        let plen1 = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(piece1)],
                &format!("{}_sz_plen1", name),
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
            .into_int_value();
        let cur_total = self
            .build_load(i64_ty, total_alloca, &format!("{}_sz_t1", name))?
            .into_int_value();
        let with_piece = self
            .builder
            .build_int_add(cur_total, plen1, &format!("{}_sz_tadd", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        // Separator before every element except the first.
        let zero = i64_ty.const_int(0, false);
        let not_first = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx1, zero, &format!("{}_sz_nf1", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let sep_add = self
            .builder
            .build_select(
                not_first,
                i64_ty.const_int(sep_len, false),
                zero,
                &format!("{}_sz_sep1", name),
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let new_total = self
            .builder
            .build_int_add(with_piece, sep_add, &format!("{}_sz_tsep", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(total_alloca, new_total)?;
        // Free the measurement piece (same runtime block ⇒ executes per iteration).
        self.flush_display_since(marker1)?;
        let next1 = self
            .builder
            .build_int_add(
                idx1,
                i64_ty.const_int(1, false),
                &format!("{}_sz_next1", name),
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx_alloca, next1)?;
        self.build_br(loop1_bb)?;
        self.builder.position_at_end(done1_bb);
        let total = self
            .build_load(i64_ty, total_alloca, &format!("{}_sz_total_ld", name))?
            .into_int_value();

        // ---- Pass 2: allocate exactly and assemble ----
        // D-4 (audit 2026-08-05): the buffer is NOT auto-registered here —
        // ownership belongs to the caller (display callers register it for
        // the print-time flush; to_json callers register it as a heap alloc
        // freed at function exit). Auto-registration double-freed JSON list
        // buffers (flush_display_frees + free_heap_allocs).
        let buf = self.malloc_or_abort(total, &format!("{}_buf", name))?;
        let off_alloca =
            self.build_alloca(BasicTypeEnum::IntType(i64_ty), &format!("{}_sz_off", name))?;
        self.build_store(off_alloca, i64_ty.const_int(0, false))?;
        // Opening bracket.
        let open_lit = self
            .builder
            .build_global_string_ptr("[", &format!("{}_sz_open", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            memcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open_lit.as_pointer_value()),
                BasicMetadataValueEnum::IntValue(i64_ty.const_int(1, false)),
            ],
            &format!("{}_sz_mcpy_open", name),
        )?;
        self.build_store(off_alloca, i64_ty.const_int(1, false))?;

        let idx2_alloca =
            self.build_alloca(BasicTypeEnum::IntType(i64_ty), &format!("{}_sz_i2", name))?;
        self.build_store(idx2_alloca, i64_ty.const_int(0, false))?;
        let loop2_bb = self
            .context
            .append_basic_block(parent, &format!("{}_sz_loop2", name));
        let body2_bb = self
            .context
            .append_basic_block(parent, &format!("{}_sz_body2", name));
        let done2_bb = self
            .context
            .append_basic_block(parent, &format!("{}_sz_done2", name));
        self.build_br(loop2_bb)?;
        self.builder.position_at_end(loop2_bb);
        let idx2 = self
            .build_load(i64_ty, idx2_alloca, &format!("{}_sz_idx2", name))?
            .into_int_value();
        let cont2 = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx2, len, &format!("{}_sz_cont2", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_cond_br(cont2, body2_bb, done2_bb)?;
        self.builder.position_at_end(body2_bb);
        // Separator before every element except the first.
        let not_first2 = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx2, zero, &format!("{}_sz_nf2", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let sep_bb = self
            .context
            .append_basic_block(parent, &format!("{}_sz_sep2", name));
        let piece2_bb = self
            .context
            .append_basic_block(parent, &format!("{}_sz_piece2", name));
        self.build_cond_br(not_first2, sep_bb, piece2_bb)?;
        self.builder.position_at_end(sep_bb);
        let sep_lit = self
            .builder
            .build_global_string_ptr(sep, &format!("{}_sz_sep", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let off_sep = self
            .build_load(i64_ty, off_alloca, &format!("{}_sz_osep", name))?
            .into_int_value();
        // SAFETY: byte-offset GEP into `buf`; off_sep + sep_len <= total by pass 1.
        let sep_dst = self
            .gep()
            .build_gep(i8_ty, buf, &[off_sep], &format!("{}_sz_sep_dst", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            memcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(sep_dst),
                BasicMetadataValueEnum::PointerValue(sep_lit.as_pointer_value()),
                BasicMetadataValueEnum::IntValue(i64_ty.const_int(sep_len, false)),
            ],
            &format!("{}_sz_mcpy_sep", name),
        )?;
        let off_sep2 = self
            .builder
            .build_int_add(
                off_sep,
                i64_ty.const_int(sep_len, false),
                &format!("{}_sz_osep2", name),
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(off_alloca, off_sep2)?;
        self.build_br(piece2_bb)?;
        self.builder.position_at_end(piece2_bb);
        let marker2 = self.display_marker();
        let piece2 = render_elem(idx2)?;
        let plen2 = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(piece2)],
                &format!("{}_sz_plen2", name),
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
            .into_int_value();
        let off_p = self
            .build_load(i64_ty, off_alloca, &format!("{}_sz_op", name))?
            .into_int_value();
        // SAFETY: byte-offset GEP into `buf`; off_p + plen2 <= total by pass 1.
        let p_dst = self
            .gep()
            .build_gep(i8_ty, buf, &[off_p], &format!("{}_sz_p_dst", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            memcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(p_dst),
                BasicMetadataValueEnum::PointerValue(piece2),
                BasicMetadataValueEnum::IntValue(plen2),
            ],
            &format!("{}_sz_mcpy_piece", name),
        )?;
        let off_p2 = self
            .builder
            .build_int_add(off_p, plen2, &format!("{}_sz_op2", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(off_alloca, off_p2)?;
        // Free this iteration's piece (same runtime block as its allocation).
        self.flush_display_since(marker2)?;
        let next2 = self
            .builder
            .build_int_add(
                idx2,
                i64_ty.const_int(1, false),
                &format!("{}_sz_next2", name),
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx2_alloca, next2)?;
        self.build_br(loop2_bb)?;
        self.builder.position_at_end(done2_bb);
        // Closing bracket + NUL terminator.
        let off_end = self
            .build_load(i64_ty, off_alloca, &format!("{}_sz_oend", name))?
            .into_int_value();
        let close_lit = self
            .builder
            .build_global_string_ptr("]", &format!("{}_sz_close", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        // SAFETY: byte-offset GEP into `buf`; off_end + 2 == total by pass 1
        // ("]" byte + trailing NUL are both included in `total`).
        let close_dst = self
            .gep()
            .build_gep(i8_ty, buf, &[off_end], &format!("{}_sz_close_dst", name))
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            memcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(close_dst),
                BasicMetadataValueEnum::PointerValue(close_lit.as_pointer_value()),
                BasicMetadataValueEnum::IntValue(i64_ty.const_int(2, false)),
            ],
            &format!("{}_sz_mcpy_close", name),
        )?;
        // `close_lit` is the global string constant "]\0": copying 2 bytes
        // writes both the ']' and the trailing NUL in one memcpy.
        Ok(buf)
    }

    pub(super) fn compile_println(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let arg_types: Vec<String> = self.pending_print_arg_types.clone();
        let i64_ty = self.context.i64_type();
        if args.is_empty() {
            let puts = self.get_runtime_fn("puts")?;
            let empty = self
                .builder
                .build_global_string_ptr("", "println_empty")
                .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
            self.build_call(
                puts,
                &[BasicMetadataValueEnum::PointerValue(
                    empty.as_pointer_value(),
                )],
                "println_empty_call",
            )?;
            return Ok(i64_ty.const_int(0, false).into());
        }
        // Single string pointer: use puts (which appends newline automatically).
        // Skip this fast path for list/record pointers, which need formatting.
        if args.len() == 1 {
            if let BasicMetadataValueEnum::PointerValue(_) = args[0] {
                let ty = arg_types.first().map(|s| s.as_str()).unwrap_or("");
                let is_list = ty.starts_with("List");
                let is_record = !ty.is_empty()
                    && self
                        .type_defs
                        .get(ty)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)));
                if !is_list && !is_record {
                    let puts = self.get_runtime_fn("puts")?;
                    self.build_call(puts, args, "puts_call")?;
                    // Q2: the single %s arg may be a display buffer — release
                    // it now that puts has consumed it.
                    self.flush_display_frees()?;
                    return Ok(i64_ty.const_int(0, false).into());
                }
            }
        }
        // Build format and arg list, handling struct/enum values by extracting the payload
        let mut print_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        let mut fmt_str = String::new();
        for (i, arg) in args.iter().enumerate() {
            // P0-3: insert a single space between adjacent args, matching
            // the interpreter's `parts.join(" ")` semantics.
            if i > 0 {
                fmt_str.push(' ');
            }
            let arg_type = arg_types.get(i).cloned().unwrap_or_default();
            let (print_arg, spec) = self.extract_print_arg(arg, i64_ty, &arg_type)?;
            print_args.push(print_arg);
            fmt_str.push_str(&spec);
        }
        fmt_str.push('\n');
        let fmt_global = self
            .builder
            .build_global_string_ptr(&fmt_str, "println_fmt")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        let mut printf_args = vec![BasicMetadataValueEnum::PointerValue(
            fmt_global.as_pointer_value(),
        )];
        printf_args.extend(print_args);
        let printf = self.get_runtime_fn("printf")?;
        self.build_call(printf, &printf_args, "printf_call")?;
        // Q2: release display buffers consumed by this printf call.
        self.flush_display_frees()?;
        Ok(i64_ty.const_int(0, false).into())
    }

    pub(super) fn extract_print_arg(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
        arg_type: &str,
    ) -> MimiResult<(BasicMetadataValueEnum<'ctx>, String)> {
        match arg {
            BasicMetadataValueEnum::StructValue(sv) => {
                let fields = sv.get_type().get_field_types();
                let num_fields = fields.len();
                // Named record: Display-like `Name { field: value, ... }`
                if !arg_type.is_empty()
                    && self
                        .type_defs
                        .get(arg_type)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
                {
                    let alloca =
                        self.build_alloca(BasicTypeEnum::StructType(sv.get_type()), "print_rec")?;
                    self.build_store(alloca, *sv)?;
                    let str_ptr = self.emit_record_display(arg_type, alloca)?;
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ));
                }
                // Custom enum: {i32 tag, i64 payload}
                if !arg_type.is_empty()
                    && self
                        .type_defs
                        .get(arg_type)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Enum(_)))
                {
                    let str_ptr = self.emit_enum_display(arg_type, *sv)?;
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ));
                }
                // Enum-like {i32, i64}: resolve type from arg_type or variant name.
                if num_fields == 2
                    && matches!(
                        fields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 32
                    )
                    && matches!(
                        fields[1],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    )
                {
                    let enum_ty = if self
                        .type_defs
                        .get(arg_type)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Enum(_)))
                    {
                        Some(arg_type.to_string())
                    } else if let Some((owner, _)) = self.find_variant_owner(arg_type) {
                        Some(owner)
                    } else {
                        None
                    };
                    if let Some(et) = enum_ty {
                        let str_ptr = self.emit_enum_display(&et, *sv)?;
                        return Ok((
                            BasicMetadataValueEnum::PointerValue(str_ptr),
                            "%s".to_string(),
                        ));
                    }
                }
                // Detect Mimi string struct: {i8*, i64}
                if num_fields == 2 && matches!(fields[0], BasicTypeEnum::PointerType(_)) {
                    let ptr = self.build_extract_value((*sv).into(), 0, "str_ptr")?;
                    match ptr {
                        BasicValueEnum::PointerValue(pv) => {
                            Ok((BasicMetadataValueEnum::PointerValue(pv), "%s".to_string()))
                        }
                        _ => Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string())),
                    }
                } else if num_fields == 2
                    && matches!(
                        fields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    )
                    && matches!(fields[1], BasicTypeEnum::PointerType(_))
                {
                    // Mimi list struct: {i64 len, ptr data} — require i64 len
                    // so Option {i1, ptr} is not misclassified as List.
                    let str_ptr = self.emit_list_typed_to_string(*sv, arg_type)?;
                    Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ))
                } else if num_fields == 2
                    && matches!(
                        fields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                    && matches!(
                        fields[1],
                        BasicTypeEnum::StructType(st) if {
                            let inner = st.get_field_types();
                            inner.len() == 2
                                && matches!(inner[0], BasicTypeEnum::PointerType(_))
                                && matches!(
                                    inner[1],
                                    BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                                )
                        }
                    )
                {
                    // Option<string> whose type name is unrecoverable (e.g. a
                    // bare `let o = if ... { None } else { Some("hi") }` where
                    // the if expression has no var_type_names entry). Route to
                    // the Option formatter: the product-tuple fallback would
                    // strlen() the None payload's null string → SIGSEGV.
                    let str_ptr = self.emit_option_to_string(*sv, None, arg_type)?;
                    Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ))
                } else if num_fields == 2
                    && matches!(
                        fields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                    && matches!(fields[1], BasicTypeEnum::PointerType(_))
                {
                    // Option with pointer payload (e.g. Option<record>):
                    // disc i1 + payload ptr. Prefer typed Option path when known.
                    let inner_rec = arg_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .filter(|inner| {
                            self.type_defs.get(*inner).is_some_and(|td| {
                                matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                            })
                        });
                    // Also when arg_type is bare "Option" but payload is a record
                    // pointer — cannot recover name; fall back to Some(%p).
                    let str_ptr = self.emit_option_to_string(*sv, inner_rec, arg_type)?;
                    Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ))
                } else if num_fields >= 2
                    && fields
                        .iter()
                        .all(|f| matches!(f, BasicTypeEnum::IntType(_)))
                    && matches!(
                        fields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                    && !arg_type.starts_with("Option")
                    && !arg_type.starts_with("Result")
                    && !self.type_defs.contains_key(arg_type)
                {
                    // Bool-headed int tuple: `(true, 1)` / map_get.
                    // Skip when arg_type is Option/Result/named enum (same layout).
                    let str_ptr = self.emit_int_tuple_to_string(*sv)?;
                    Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ))
                } else if num_fields >= 2 {
                    // Option/Result/enum-like: print payload field (field 1).
                    // For Option None (disc=0), interp prints `None()` — approximate
                    // by printing payload only when disc!=0, else "None".
                    if (arg_type.starts_with("Option") || arg_type == "Option")
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        )
                    {
                        let inner_rec = arg_type
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                            .filter(|inner| {
                                self.type_defs.get(*inner).is_some_and(|td| {
                                    matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                })
                            });
                        let str_ptr = self.emit_option_to_string(*sv, inner_rec, arg_type)?;
                        return Ok((
                            BasicMetadataValueEnum::PointerValue(str_ptr),
                            "%s".to_string(),
                        ));
                    }
                    if (arg_type.starts_with("Result") || arg_type == "Result")
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        )
                        && num_fields >= 3
                    {
                        let ok_rec = arg_type
                            .strip_prefix("Result<")
                            .and_then(|s| s.split(',').next())
                            .map(|s| s.trim())
                            .filter(|inner| {
                                !inner.is_empty()
                                    && self.type_defs.get(*inner).is_some_and(|td| {
                                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                    })
                            });
                        let str_ptr = self.emit_result_to_string_typed(*sv, ok_rec, arg_type)?;
                        return Ok((
                            BasicMetadataValueEnum::PointerValue(str_ptr),
                            "%s".to_string(),
                        ));
                    }
                    // Heterogeneous product / user tuple: format all fields.
                    // Skip named enums (i32 tag + payload) already handled above.
                    // Type aliases of product tuples (e.g. `type Pair = (i32,i32)`)
                    // must use the product path — they appear in type_defs but are
                    // not named records/enums.
                    let is_product_alias = self.is_product_tuple_alias(arg_type);
                    let is_named = !arg_type.is_empty()
                        && self.type_defs.contains_key(arg_type)
                        && !is_product_alias;
                    let is_enum_layout = num_fields == 2
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 32
                        )
                        && matches!(fields[1], BasicTypeEnum::IntType(t) if t.get_bit_width() == 64);
                    if (!is_named || is_product_alias) && !is_enum_layout {
                        let str_ptr = self.emit_product_tuple_to_string(*sv, Some(arg_type))?;
                        return Ok((
                            BasicMetadataValueEnum::PointerValue(str_ptr),
                            "%s".to_string(),
                        ));
                    }
                    let payload = self.build_extract_value((*sv).into(), 1, "payload")?;
                    match payload {
                        BasicValueEnum::IntValue(iv) => {
                            let ext = if iv.get_type().get_bit_width() < 64 {
                                if iv.get_type().get_bit_width() == 1 {
                                    self.builder
                                        .build_int_z_extend(iv, i64_ty, "payload_zext")
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                } else {
                                    self.builder
                                        .build_int_s_extend(iv, i64_ty, "payload_sext")
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                }
                            } else {
                                iv
                            };
                            Ok((BasicMetadataValueEnum::IntValue(ext), "%ld".to_string()))
                        }
                        _ => Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string())),
                    }
                } else {
                    Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string()))
                }
            }
            BasicMetadataValueEnum::PointerValue(pv) => {
                let pv = *pv;
                if arg_type.starts_with("List") {
                    // The pointer points to a list struct alloca; load it and
                    // reuse the struct formatting path above.
                    let list_struct_ty = self.list_struct_type();
                    let loaded = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(list_struct_ty),
                            pv,
                            "print_list_ptr_load",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    return self.extract_print_arg(
                        &BasicMetadataValueEnum::StructValue(loaded.into_struct_value()),
                        i64_ty,
                        arg_type,
                    );
                }
                // Named record stored as pointer to struct alloca.
                if !arg_type.is_empty()
                    && self
                        .type_defs
                        .get(arg_type)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
                {
                    let str_ptr = self.emit_record_display(arg_type, pv)?;
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ));
                }
                // Map/Set opaque i64 handle may arrive as int; pointer is C string.
                Ok((BasicMetadataValueEnum::PointerValue(pv), "%s".to_string()))
            }
            BasicMetadataValueEnum::IntValue(iv) => {
                // Map/Set opaque handles: serialize via runtime JSON helpers.
                if arg_type == "Map" || arg_type.starts_with("Map<") {
                    // Map of product-tuple values: decode heap ValueHandles.
                    if let Some(val_ty) = arg_type
                        .strip_prefix("Map<string, ")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if let Some((runtime_fn, product_type)) =
                            self.resolve_container_product(val_ty, "mimi_map_to_json")
                        {
                            let raw = self.emit_map_container_product_to_json(
                                *iv,
                                &runtime_fn,
                                &product_type,
                                1,
                            )?;
                            return Ok((
                                BasicMetadataValueEnum::PointerValue(raw),
                                "%s".to_string(),
                            ));
                        }
                    }
                    // Wave-1 audit fix (§8): route on the parsed map value
                    // type (full outer match), not substring `contains`.
                    let fn_name = Self::map_json_fn_for_type(arg_type);
                    let func = self.get_runtime_fn(fn_name)?;
                    let raw = self
                        .build_call(
                            func,
                            &[BasicMetadataValueEnum::IntValue(*iv)],
                            "print_map_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("map to_json void")?
                        .into_pointer_value();
                    return Ok((BasicMetadataValueEnum::PointerValue(raw), "%s".to_string()));
                }
                if arg_type == "Set" || arg_type.starts_with("Set<") || arg_type == "set" {
                    if let Some(elem) = arg_type
                        .strip_prefix("Set<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if let Some((runtime_fn, product_type)) =
                            self.resolve_container_product(elem, "mimi_set_to_json")
                        {
                            let raw = self.emit_map_container_product_to_json(
                                *iv,
                                &runtime_fn,
                                &product_type,
                                1,
                            )?;
                            return Ok((
                                BasicMetadataValueEnum::PointerValue(raw),
                                "%s".to_string(),
                            ));
                        }
                    }
                    // Wave-1 audit fix (§8): route on the parsed element type
                    // (full outer match), not substring `contains`.
                    let fn_name = Self::set_display_fn_for_type(arg_type);
                    let func = self.get_runtime_fn(fn_name)?;
                    let raw = self
                        .build_call(
                            func,
                            &[BasicMetadataValueEnum::IntValue(*iv)],
                            "print_set_disp",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("set display void")?
                        .into_pointer_value();
                    return Ok((BasicMetadataValueEnum::PointerValue(raw), "%s".to_string()));
                }
                // A1: Ensure integer is i64 for printf("%ld").
                // i1 bool OR typed `bool` (is_ok/is_err return i64 0/1): print
                // "true"/"false" to match interpreter Display.
                let bw = iv.get_type().get_bit_width();
                let as_bool = bw == 1 || arg_type == "bool" || arg_type == "Bool";
                if as_bool {
                    let true_g = self
                        .builder
                        .build_global_string_ptr("true", "print_bool_true")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let false_g = self
                        .builder
                        .build_global_string_ptr("false", "print_bool_false")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let zero = iv.get_type().const_int(0, false);
                    let is_true = self
                        .builder
                        .build_int_compare(IntPredicate::NE, *iv, zero, "print_bool")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let selected = self
                        .builder
                        .build_select(
                            is_true,
                            true_g.as_pointer_value(),
                            false_g.as_pointer_value(),
                            "print_bool_str",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(selected.into_pointer_value()),
                        "%s".to_string(),
                    ));
                }
                let ext_iv = if bw < 64 {
                    self.builder
                        .build_int_s_extend(*iv, i64_ty, "print_sext")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    *iv
                };
                Ok((BasicMetadataValueEnum::IntValue(ext_iv), "%ld".to_string()))
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                // Wave-1 audit fix (§8, FIX: `%g` prints only 6 significant
                // digits — e.g. "1.23457e+08" for 123456789.123456789):
                // route scalar float printing through `mimi_to_string_f64`
                // (Rust `{}` shortest round-trip Display), exactly matching
                // the VM's formatting. The heap string is registered so the
                // consuming print's `flush_display_frees` releases it.
                let to_f64_fn = self.get_runtime_fn("mimi_to_string_f64")?;
                let str_ptr = self
                    .build_call(
                        to_f64_fn,
                        &[BasicMetadataValueEnum::FloatValue(*fv)],
                        "print_f64_str",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_to_string_f64 returned void")?
                    .into_pointer_value();
                self.register_display_alloc(str_ptr);
                Ok((
                    BasicMetadataValueEnum::PointerValue(str_ptr),
                    "%s".to_string(),
                ))
            }
            _ => Ok((*arg, "%p".to_string())),
        }
    }

    /// Format `List<Map>` as `[{"a":1}, {"b":2}]` via map JSON helpers.
    fn emit_list_map_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        map_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_map_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        // D-4: exact-size two-pass assembly (fixed 4096-byte strcat removed).
        // The element renderer is pure, so the two passes render each element
        // twice; measurement pieces are freed per iteration by the sized helper.
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                let data_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 1, "list_map_data_gep")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let data_ptr = self
                    .build_load(i8_ptr, data_gep, "list_map_data")?
                    .into_pointer_value();
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // slots, loaded from the struct's data field). The sized
                // helper's loop guards `idx ULT len` gate every call.
                let elem_slot = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], "list_map_slot")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let handle = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_map_handle")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let map_str = if let Some(val_ty) = map_type
                    .strip_prefix("Map<string, ")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                        let elem = if self.is_product_tuple_alias(val_ty) {
                            self.resolve_alias_type_name(val_ty)
                        } else {
                            val_ty.to_string()
                        };
                        // Display style for println of List<Map product>.
                        self.emit_map_product_to_json(handle, &elem, 1)?
                    } else if let Some(opt_elem) = val_ty
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if opt_elem.starts_with('(') || self.is_product_tuple_alias(opt_elem) {
                            let elem = if self.is_product_tuple_alias(opt_elem) {
                                self.resolve_alias_type_name(opt_elem)
                            } else {
                                opt_elem.to_string()
                            };
                            self.emit_map_option_product_to_json(handle, &elem, 1)?
                        } else {
                            let map_fn = self.get_runtime_fn("mimi_map_to_json_i64")?;
                            self.build_call(
                                map_fn,
                                &[BasicMetadataValueEnum::IntValue(handle)],
                                "list_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("map to_json void")?
                            .into_pointer_value()
                        }
                    } else if val_ty.starts_with("Result<") {
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
                                self.emit_map_result_product_to_json(handle, &elem, 1)?
                            } else {
                                let map_fn = self.get_runtime_fn("mimi_map_to_json_i64")?;
                                self.build_call(
                                    map_fn,
                                    &[BasicMetadataValueEnum::IntValue(handle)],
                                    "list_map_json",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("map to_json void")?
                                .into_pointer_value()
                            }
                        } else {
                            let map_fn = self.get_runtime_fn("mimi_map_to_json_i64")?;
                            self.build_call(
                                map_fn,
                                &[BasicMetadataValueEnum::IntValue(handle)],
                                "list_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("map to_json void")?
                            .into_pointer_value()
                        }
                    } else if let Some(set_elem) = val_ty
                        .strip_prefix("Set<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                            let elem = if self.is_product_tuple_alias(set_elem) {
                                self.resolve_alias_type_name(set_elem)
                            } else {
                                set_elem.to_string()
                            };
                            self.emit_map_set_product_to_json(handle, &elem, 1)?
                        } else {
                            let map_fn = self.get_runtime_fn("mimi_map_to_json_i64")?;
                            self.build_call(
                                map_fn,
                                &[BasicMetadataValueEnum::IntValue(handle)],
                                "list_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("map to_json void")?
                            .into_pointer_value()
                        }
                    } else if let Some(list_elem) = val_ty
                        .strip_prefix("List<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if list_elem.starts_with('(') || self.is_product_tuple_alias(list_elem) {
                            let elem = if self.is_product_tuple_alias(list_elem) {
                                self.resolve_alias_type_name(list_elem)
                            } else {
                                list_elem.to_string()
                            };
                            self.emit_map_list_product_to_json(handle, &elem, 1)?
                        } else {
                            let map_fn = self.get_runtime_fn("mimi_map_to_json_i64")?;
                            self.build_call(
                                map_fn,
                                &[BasicMetadataValueEnum::IntValue(handle)],
                                "list_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("map to_json void")?
                            .into_pointer_value()
                        }
                    } else if val_ty.starts_with("Map<string, ") {
                        if let Some(inner_val) = val_ty
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
                                self.emit_map_map_product_to_json(handle, &elem, 1)?
                            } else {
                                let map_fn = self.get_runtime_fn("mimi_map_to_json_i64")?;
                                self.build_call(
                                    map_fn,
                                    &[BasicMetadataValueEnum::IntValue(handle)],
                                    "list_map_json",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("map to_json void")?
                                .into_pointer_value()
                            }
                        } else {
                            let map_fn = self.get_runtime_fn("mimi_map_to_json_i64")?;
                            self.build_call(
                                map_fn,
                                &[BasicMetadataValueEnum::IntValue(handle)],
                                "list_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("map to_json void")?
                            .into_pointer_value()
                        }
                    } else {
                        // Wave-1 audit fix (§8): route on the parsed value type,
                        // not substring `contains`.
                        let map_fn_name = Self::map_json_fn_for_type(map_type);
                        let map_fn = self.get_runtime_fn(map_fn_name)?;
                        self.build_call(
                            map_fn,
                            &[BasicMetadataValueEnum::IntValue(handle)],
                            "list_map_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("map to_json void")?
                        .into_pointer_value()
                    }
                } else {
                    let map_fn = self.get_runtime_fn("mimi_map_to_json_i64")?;
                    self.build_call(
                        map_fn,
                        &[BasicMetadataValueEnum::IntValue(handle)],
                        "list_map_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("map to_json void")?
                    .into_pointer_value()
                };
                Ok(map_str)
            },
            "list_map",
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Format `List<Set>` as `[Set{…}, ...]` via set display helpers.
    fn emit_list_set_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        set_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_set_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        // D-4: exact-size two-pass assembly (fixed 4096-byte strcat removed).
        // The element renderer is pure, so the two passes render each element
        // twice; measurement pieces are freed per iteration by the sized helper.
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                let data_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 1, "list_set_data_gep")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let data_ptr = self
                    .build_load(i8_ptr, data_gep, "list_set_data")?
                    .into_pointer_value();
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // slots, loaded from the struct's data field). The sized
                // helper's loop guards `idx ULT len` gate every call.
                let elem_slot = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], "list_set_slot")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let handle = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_set_handle")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let set_str = if let Some(elem) = set_type
                    .strip_prefix("Set<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                        let resolved = if self.is_product_tuple_alias(elem) {
                            self.resolve_alias_type_name(elem)
                        } else {
                            elem.to_string()
                        };
                        self.emit_set_product_to_json(handle, &resolved, 1)?
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
                                    self.get_runtime_fn("mimi_set_to_json_map_product_i64")?;
                                self.build_call(
                                    func,
                                    &[
                                        BasicMetadataValueEnum::IntValue(handle),
                                        BasicMetadataValueEnum::IntValue(
                                            i64_ty.const_int(arity as u64, false),
                                        ),
                                        BasicMetadataValueEnum::IntValue(
                                            i64_ty.const_int(1, false),
                                        ),
                                    ],
                                    "list_set_map_disp",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("set map product display void")?
                                .into_pointer_value()
                            } else {
                                let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                                self.build_call(
                                    set_fn,
                                    &[BasicMetadataValueEnum::IntValue(handle)],
                                    "list_set_disp",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("set display void")?
                                .into_pointer_value()
                            }
                        } else {
                            let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                            self.build_call(
                                set_fn,
                                &[BasicMetadataValueEnum::IntValue(handle)],
                                "list_set_disp",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("set display void")?
                            .into_pointer_value()
                        }
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
                            self.emit_set_option_product_to_json(handle, &resolved, 1)?
                        } else {
                            let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                            self.build_call(
                                set_fn,
                                &[BasicMetadataValueEnum::IntValue(handle)],
                                "list_set_disp",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("set display void")?
                            .into_pointer_value()
                        }
                    } else if elem.starts_with("Result<") {
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
                                self.emit_set_result_product_to_json(handle, &resolved, 1)?
                            } else {
                                let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                                self.build_call(
                                    set_fn,
                                    &[BasicMetadataValueEnum::IntValue(handle)],
                                    "list_set_disp",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("set display void")?
                                .into_pointer_value()
                            }
                        } else {
                            let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                            self.build_call(
                                set_fn,
                                &[BasicMetadataValueEnum::IntValue(handle)],
                                "list_set_disp",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("set display void")?
                            .into_pointer_value()
                        }
                    } else {
                        // Wave-1 audit fix (§8): route on the parsed element type,
                        // not substring `contains`.
                        let set_fn_name = Self::set_display_fn_for_type(set_type);
                        let set_fn = self.get_runtime_fn(set_fn_name)?;
                        self.build_call(
                            set_fn,
                            &[BasicMetadataValueEnum::IntValue(handle)],
                            "list_set_disp",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("set display void")?
                        .into_pointer_value()
                    }
                } else {
                    let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                    self.build_call(
                        set_fn,
                        &[BasicMetadataValueEnum::IntValue(handle)],
                        "list_set_disp",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("set display void")?
                    .into_pointer_value()
                };
                Ok(set_str)
            },
            "list_set",
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Serialize `List<Result<Option<(…)>,E>>` with full Result layout.
    /// List of Set of Option of product-tuple values.
    pub(in crate::codegen) fn emit_list_set_option_product_to_json(
        &self,
        list_ptr: inkwell::values::PointerValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_list_set_option_product_to_json")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(list_ptr),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "list_set_option_product_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("list set option product to_json void")?
            .into_pointer_value())
    }

    /// List of Set of Result of product-tuple values.
    /// List of Set of Map of product-tuple values.
    pub(in crate::codegen) fn emit_list_set_map_product_to_json(
        &self,
        list_ptr: inkwell::values::PointerValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_list_set_map_product_to_json")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(list_ptr),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "list_set_map_product_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("list set map product to_json void")?
            .into_pointer_value())
    }

    pub(in crate::codegen) fn emit_list_set_result_product_to_json(
        &self,
        list_ptr: inkwell::values::PointerValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_list_set_result_product_to_json")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(list_ptr),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "list_set_result_product_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("list set result product to_json void")?
            .into_pointer_value())
    }

    pub(in crate::codegen) fn emit_list_result_option_product_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        elem_res_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let res_ty = crate::codegen::extract_list_elem_type(&format!("List<{}>", elem_res_type))
            .unwrap_or_else(|| {
                crate::ast::Type::Name(
                    "Result".into(),
                    vec![
                        crate::ast::Type::Option(Box::new(crate::ast::Type::Tuple(vec![
                            crate::ast::Type::Name("i32".into(), vec![]),
                            crate::ast::Type::Name("i32".into(), vec![]),
                        ]))),
                        crate::ast::Type::Name("string".into(), vec![]),
                    ],
                )
            });
        let res_sty = match self.llvm_type_for(&res_ty) {
            Some(BasicTypeEnum::StructType(s)) => s,
            _ => {
                return Err(CompileError::Generic(
                    "to_json List of Result of Option of tuple: cannot map Result layout".into(),
                ));
            }
        };
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_res_opt_prod_json_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_res_opt_prod_json_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        // D-4: exact-size two-pass assembly instead of the fixed 8192-byte
        // strcat loop. Every heap piece produced below is registered on the
        // display ledger; the sized helper flushes this iteration's pieces
        // right after measuring/copying them (the old loop leaked each
        // element's piece on every iteration).
        let buf = self.emit_sized_list_of_pieces(
            len,
            ",",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_slot = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_res_opt_prod_json_slot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let elem_i64 = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_res_opt_prod_json_elem")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let res_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_res_opt_prod_json_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let loaded = self
                    .builder
                    .build_load(
                        BasicTypeEnum::StructType(res_sty),
                        res_ptr,
                        "list_res_opt_prod_json_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                let disc = self
                    .build_extract_value(loaded.into(), 0, "list_res_opt_prod_disc")?
                    .into_int_value();
                let is_ok = self
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        disc,
                        disc.get_type().const_int(0, false),
                        "list_res_opt_prod_is_ok",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let ok_bb = self
                    .context
                    .append_basic_block(parent, "list_res_opt_prod_json_ok");
                let err_bb = self
                    .context
                    .append_basic_block(parent, "list_res_opt_prod_json_err");
                let merge_bb = self
                    .context
                    .append_basic_block(parent, "list_res_opt_prod_json_merge");
                let piece_slot = self.build_alloca(
                    BasicTypeEnum::PointerType(i8_ptr),
                    "list_res_opt_prod_piece",
                )?;
                self.builder
                    .build_conditional_branch(is_ok, ok_bb, err_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(ok_bb);
                // Branch-local scratch management: every temporary is
                // registered after `arm_marker` and freed by the branch's own
                // flush below, so no undef pointer ever reaches the sized
                // helper's unconditional flush (a fixed-branch-free undef
                // would be free(garbage)). Only the final piece crosses the
                // merge, where it is defined on every path.
                let arm_marker = self.display_marker();
                let ok_pay = self
                    .build_extract_value(loaded.into(), 1, "list_res_opt_prod_ok")?
                    .into_struct_value();
                // Ok is Option {i1, payload}.
                let o_disc = self
                    .build_extract_value(ok_pay.into(), 0, "list_res_opt_prod_o_disc")?
                    .into_int_value();
                let is_some = self
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        o_disc,
                        o_disc.get_type().const_int(0, false),
                        "list_res_opt_prod_is_some",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let some_bb = self
                    .context
                    .append_basic_block(parent, "list_res_opt_prod_json_some");
                let none_bb = self
                    .context
                    .append_basic_block(parent, "list_res_opt_prod_json_none");
                let ok_merge = self
                    .context
                    .append_basic_block(parent, "list_res_opt_prod_json_ok_m");
                self.builder
                    .build_conditional_branch(is_some, some_bb, none_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(some_bb);
                let o_pay =
                    self.build_extract_value(ok_pay.into(), 1, "list_res_opt_prod_o_pay")?;
                let pay_json = if let BasicValueEnum::StructValue(pay_sv) = o_pay {
                    let p = self.emit_product_tuple_to_json(pay_sv)?;
                    self.register_display_alloc(p);
                    p
                } else {
                    let tmp =
                        self.malloc_or_abort(i64_ty.const_int(8, false), "list_res_opt_prod_zero")?;
                    let zero_lit = self
                        .builder
                        .build_global_string_ptr("0", "list_res_opt_prod_zero_lit")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let strcpy_fn = self.get_runtime_fn("strcpy")?;
                    self.build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(tmp),
                            BasicMetadataValueEnum::PointerValue(zero_lit.as_pointer_value()),
                        ],
                        "list_res_opt_prod_zero_cpy",
                    )?;
                    self.register_display_alloc(tmp);
                    tmp
                };
                let some_inner = self.sized_cat_parts(
                    &[
                        CatPart::Lit("{\"Some\":["),
                        CatPart::Dyn(pay_json),
                        CatPart::Lit("]}"),
                    ],
                    "list_res_opt_prod_some_i",
                    true,
                )?;
                // `ok_wrap` is the final piece — allocated after the branch
                // flush so only it (and some_inner, via register above)
                // survives the arm_marker flush below.
                let ok_wrap = self.sized_cat_parts(
                    &[
                        CatPart::Lit("{\"Ok\":["),
                        CatPart::Dyn(some_inner),
                        CatPart::Lit("]}"),
                    ],
                    "list_res_opt_prod_ok_w",
                    false,
                )?;
                self.flush_display_since(arm_marker)?;
                self.build_store(piece_slot, ok_wrap)?;
                self.builder
                    .build_unconditional_branch(ok_merge)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(none_bb);
                let none_ok =
                    self.malloc_or_abort(i64_ty.const_int(32, false), "list_res_opt_prod_none_ok")?;
                let nfmt = self
                    .builder
                    .build_global_string_ptr("{\"Ok\":[\"None\"]}", "list_res_opt_prod_nfmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                self.build_call(
                    strcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(none_ok),
                        BasicMetadataValueEnum::PointerValue(nfmt.as_pointer_value()),
                    ],
                    "list_res_opt_prod_ncpy",
                )?;
                self.build_store(piece_slot, none_ok)?;
                self.builder
                    .build_unconditional_branch(ok_merge)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(ok_merge);
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(err_bb);
                let err_pay =
                    self.build_extract_value(loaded.into(), 2, "list_res_opt_prod_err")?;
                let err_i64 = match err_pay {
                    BasicValueEnum::IntValue(iv) => {
                        if iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(iv, i64_ty, "list_res_opt_prod_err_i64")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            iv
                        }
                    }
                    _ => i64_ty.const_int(0, false),
                };
                let ewrap = self.emit_result_err_json(err_i64, true)?;
                self.build_store(piece_slot, ewrap)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let piece = self
                    .build_load(
                        BasicTypeEnum::PointerType(i8_ptr),
                        piece_slot,
                        "list_res_opt_prod_piece_ld",
                    )?
                    .into_pointer_value();
                // Defined on every path (each arm stored it); register so the
                // sized helper's per-iteration flush frees it exactly once.
                self.register_display_alloc(piece);
                Ok(piece)
            },
            "list_res_opt_prod_json",
        )?;
        // Ownership of the assembled JSON list buffer stays with the caller:
        // simple.rs registers it via register_heap_alloc (freed at function
        // exit). NOT a display buffer — do not register it here.
        Ok(buf)
    }

    /// Serialize `List<Result<(…),E>>` / `List<Result<Record,E>>` to JSON.
    pub(in crate::codegen) fn emit_list_result_product_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        elem_res_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let res_ty = crate::codegen::extract_list_elem_type(&format!("List<{}>", elem_res_type))
            .unwrap_or_else(|| {
                crate::ast::Type::Name(
                    "Result".into(),
                    vec![
                        crate::ast::Type::Tuple(vec![
                            crate::ast::Type::Name("i32".into(), vec![]),
                            crate::ast::Type::Name("i32".into(), vec![]),
                        ]),
                        crate::ast::Type::Name("string".into(), vec![]),
                    ],
                )
            });
        let res_sty = match self.llvm_type_for(&res_ty) {
            Some(BasicTypeEnum::StructType(s)) => s,
            _ => {
                return Err(CompileError::Generic(
                    "to_json List of Result of tuple: cannot map Result layout".into(),
                ));
            }
        };
        let ok_inner = elem_res_type
            .strip_prefix("Result<")
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let is_named_record = self
            .type_defs
            .get(&ok_inner)
            .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)));
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_res_prod_json_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_res_prod_json_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        // D-4: exact-size two-pass assembly instead of the fixed 8192-byte
        // strcat loop. Result has an Ok/Err branch; scratch is managed inside
        // each arm (arm_marker + flush) so no undef pointer reaches the sized
        // helper's unconditional flush. Only the piece crosses the merge,
        // where it is defined on every path.
        self.emit_sized_list_of_pieces(
            len,
            ",",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_slot = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_res_prod_json_slot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let elem_i64 = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_res_prod_json_elem")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let res_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_res_prod_json_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let loaded = self
                    .builder
                    .build_load(
                        BasicTypeEnum::StructType(res_sty),
                        res_ptr,
                        "list_res_prod_json_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                let disc = self
                    .build_extract_value(loaded.into(), 0, "list_res_prod_disc")?
                    .into_int_value();
                let is_ok = self
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        disc,
                        disc.get_type().const_int(0, false),
                        "list_res_prod_is_ok",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let ok_bb = self
                    .context
                    .append_basic_block(parent, "list_res_prod_json_ok");
                let err_bb = self
                    .context
                    .append_basic_block(parent, "list_res_prod_json_err");
                let merge_bb = self
                    .context
                    .append_basic_block(parent, "list_res_prod_json_merge");
                let piece_slot =
                    self.build_alloca(BasicTypeEnum::PointerType(i8_ptr), "list_res_prod_piece")?;
                self.builder
                    .build_conditional_branch(is_ok, ok_bb, err_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(ok_bb);
                let arm_marker = self.display_marker();
                let ok_pay = self.build_extract_value(loaded.into(), 1, "list_res_prod_ok")?;
                let piece = if let BasicValueEnum::StructValue(ok_sv) = ok_pay {
                    let ok_fields = ok_sv.get_type().get_field_types();
                    let ok_is_nested_result = ok_fields.len() >= 3
                        && matches!(
                            ok_fields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        );
                    let ok_json = if ok_is_nested_result {
                        self.emit_result_struct_to_json_cstr(ok_sv, &ok_inner)?
                    } else if is_named_record {
                        let rec_ty = ok_sv.get_type();
                        let rec_alloca = self
                            .build_alloca(BasicTypeEnum::StructType(rec_ty), "list_res_rec_tmp")?;
                        self.build_store(rec_alloca, ok_sv)?;
                        self.compile_record_to_json_cstr(&ok_inner, rec_alloca)?
                    } else {
                        self.emit_product_tuple_to_json(ok_sv)?
                    };
                    // Sub-emitters return unregistered buffers; register so
                    // the arm flush below frees them after the wrap consumes.
                    self.register_display_alloc(ok_json);
                    // Sized wrap instead of fixed 1024 snprintf; it is the
                    // piece, allocated after the arm flush point.
                    let wrap = self.sized_cat_parts(
                        &[
                            CatPart::Lit("{\"Ok\":["),
                            CatPart::Dyn(ok_json),
                            CatPart::Lit("]}"),
                        ],
                        "list_res_prod_wrap",
                        false,
                    )?;
                    self.flush_display_since(arm_marker)?;
                    wrap
                } else {
                    let ok_i64 = match ok_pay {
                        BasicValueEnum::IntValue(iv) => {
                            if iv.get_type().get_bit_width() < 64 {
                                self.builder
                                    .build_int_s_extend(iv, i64_ty, "list_res_prod_ok_i64")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            } else {
                                iv
                            }
                        }
                        _ => i64_ty.const_int(0, false),
                    };
                    let wrap = self
                        .malloc_or_abort(i64_ty.const_int(64, false), "list_res_prod_i64_wrap")?;
                    let fmt = self
                        .builder
                        .build_global_string_ptr("{\"Ok\":[%ld]}", "list_res_prod_i64_fmt")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let snprintf_fn = self.get_runtime_fn("snprintf")?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(wrap),
                            BasicMetadataValueEnum::IntValue(i64_ty.const_int(64, false)),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(ok_i64),
                        ],
                        "list_res_prod_i64_sn",
                    )?;
                    wrap
                };
                self.build_store(piece_slot, piece)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(err_bb);
                let err_pay = self.build_extract_value(loaded.into(), 2, "list_res_prod_err")?;
                let err_i64 = match err_pay {
                    BasicValueEnum::IntValue(iv) => {
                        if iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(iv, i64_ty, "list_res_prod_err_i64")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            iv
                        }
                    }
                    _ => i64_ty.const_int(0, false),
                };
                // Heapish → string Err JSON; else numeric Err.
                let ewrap = self.emit_result_err_json(err_i64, true)?;
                self.build_store(piece_slot, ewrap)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let piece = self
                    .build_load(
                        BasicTypeEnum::PointerType(i8_ptr),
                        piece_slot,
                        "list_res_prod_piece_ld",
                    )?
                    .into_pointer_value();
                // Defined on every path; register so the sized helper's
                // per-iteration flush frees it exactly once.
                self.register_display_alloc(piece);
                Ok(piece)
            },
            "list_res_prod_json",
        )
    }

    /// Serialize `List<Option<(…)>>` / `List<Option<Record>>` (ptrtoint of full
    /// Option layout) to JSON.
    pub(in crate::codegen) fn emit_list_option_product_tuple_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        elem_opt_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let opt_ty = crate::codegen::extract_list_elem_type(&format!("List<{}>", elem_opt_type))
            .unwrap_or_else(|| {
                crate::ast::Type::Name(
                    "Option".into(),
                    vec![crate::ast::Type::Tuple(vec![
                        crate::ast::Type::Name("i32".into(), vec![]),
                        crate::ast::Type::Name("i32".into(), vec![]),
                    ])],
                )
            });
        let opt_sty = match self.llvm_type_for(&opt_ty) {
            Some(BasicTypeEnum::StructType(s)) => s,
            _ => {
                return Err(CompileError::Generic(
                    "to_json List of Option of tuple: cannot map Option layout".into(),
                ));
            }
        };
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_opt_tup_json_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_opt_tup_json_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        // D-4: exact-size two-pass assembly instead of the fixed 8192-byte
        // strcat loop. Option has a Some/None branch; scratch is managed inside
        // the Some arm (arm_marker + flush) so no undef pointer reaches the
        // sized helper's unconditional flush. The None arm allocates its piece
        // (a static global would be freed by the helper's flush). Only the
        // piece crosses the merge, defined on every path.
        self.emit_sized_list_of_pieces(
            len,
            ",",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_slot = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_opt_tup_json_slot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let elem_i64 = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_opt_tup_json_elem")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let opt_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_opt_tup_json_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let loaded = self
                    .builder
                    .build_load(
                        BasicTypeEnum::StructType(opt_sty),
                        opt_ptr,
                        "list_opt_tup_json_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                let disc = self
                    .build_extract_value(loaded.into(), 0, "list_opt_tup_disc")?
                    .into_int_value();
                let is_some = self
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        disc,
                        disc.get_type().const_int(0, false),
                        "list_opt_tup_is_some",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let some_bb = self
                    .context
                    .append_basic_block(parent, "list_opt_tup_json_some");
                let none_bb = self
                    .context
                    .append_basic_block(parent, "list_opt_tup_json_none");
                let merge_bb = self
                    .context
                    .append_basic_block(parent, "list_opt_tup_json_merge");
                let piece_slot =
                    self.build_alloca(BasicTypeEnum::PointerType(i8_ptr), "list_opt_tup_piece")?;
                self.builder
                    .build_conditional_branch(is_some, some_bb, none_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(some_bb);
                let arm_marker = self.display_marker();
                let pay = self.build_extract_value(loaded.into(), 1, "list_opt_tup_pay")?;
                let inner_name = elem_opt_type
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                    .unwrap_or("");
                let is_named_record = self
                    .type_defs
                    .get(inner_name)
                    .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)));
                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                let piece = if let BasicValueEnum::StructValue(pay_sv) = pay {
                    let pfields = pay_sv.get_type().get_field_types();
                    let is_result_layout = pfields.len() >= 3
                        && matches!(
                            pfields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        );
                    let is_list_layout = pfields.len() == 2
                        && matches!(
                            pfields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        )
                        && matches!(pfields[1], BasicTypeEnum::PointerType(_));
                    let pay_json = if is_result_layout {
                        self.emit_result_struct_to_json_cstr(pay_sv, inner_name)?
                    } else if is_list_layout {
                        let tmp = self.build_alloca(
                            BasicTypeEnum::StructType(pay_sv.get_type()),
                            "list_opt_list_tmp",
                        )?;
                        self.build_store(tmp, pay_sv)?;
                        self.emit_list_payload_to_json_cstr(tmp, inner_name)?
                    } else if is_named_record {
                        let rec_ty = pay_sv.get_type();
                        let rec_alloca = self
                            .build_alloca(BasicTypeEnum::StructType(rec_ty), "list_opt_rec_tmp")?;
                        self.build_store(rec_alloca, pay_sv)?;
                        self.compile_record_to_json_cstr(inner_name, rec_alloca)?
                    } else {
                        self.emit_product_tuple_to_json(pay_sv)?
                    };
                    // Sub-emitters return unregistered buffers; register so
                    // the arm flush below frees them after the wrap consumes.
                    self.register_display_alloc(pay_json);
                    // Sized wrap instead of fixed 1024 snprintf; it is the
                    // piece, allocated after the arm flush point.
                    let wrap = self.sized_cat_parts(
                        &[
                            CatPart::Lit("{\"Some\":["),
                            CatPart::Dyn(pay_json),
                            CatPart::Lit("]}"),
                        ],
                        "list_opt_tup_wrap",
                        false,
                    )?;
                    self.flush_display_since(arm_marker)?;
                    wrap
                } else if let BasicValueEnum::IntValue(iv) = pay {
                    let pay_i64 = if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(iv, i64_ty, "list_opt_tup_pay_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    };
                    if inner_name.starts_with("List") {
                        let list_ptr = self
                            .builder
                            .build_int_to_ptr(pay_i64, i8_ptr, "list_opt_list_ptr")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let pay_json = self.emit_list_payload_to_json_cstr(list_ptr, inner_name)?;
                        self.register_display_alloc(pay_json);
                        // Sized wrap instead of fixed 1024 snprintf; piece.
                        let wrap = self.sized_cat_parts(
                            &[
                                CatPart::Lit("{\"Some\":["),
                                CatPart::Dyn(pay_json),
                                CatPart::Lit("]}"),
                            ],
                            "list_opt_list_wrap",
                            false,
                        )?;
                        self.flush_display_since(arm_marker)?;
                        wrap
                    } else {
                        let wrap =
                            self.malloc_or_abort(i64_ty.const_int(64, false), "list_opt_i64_wrap")?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr("{\"Some\":[%ld]}", "list_opt_i64_some_fmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(wrap),
                                BasicMetadataValueEnum::IntValue(i64_ty.const_int(64, false)),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(pay_i64),
                            ],
                            "list_opt_i64_some_sn",
                        )?;
                        wrap
                    }
                } else {
                    let wrap =
                        self.malloc_or_abort(i64_ty.const_int(16, false), "list_opt_null_wrap")?;
                    let lit = self
                        .builder
                        .build_global_string_ptr("{\"Some\":[null]}", "list_opt_null_lit")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let strcpy_fn = self.get_runtime_fn("strcpy")?;
                    self.build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(wrap),
                            BasicMetadataValueEnum::PointerValue(lit.as_pointer_value()),
                        ],
                        "list_opt_null_cpy",
                    )?;
                    wrap
                };
                self.build_store(piece_slot, piece)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(none_bb);
                // None piece must be heap-allocated (the sized helper's
                // per-iteration flush frees every piece unconditionally).
                let none_wrap =
                    self.malloc_or_abort(i64_ty.const_int(16, false), "list_opt_none_wrap")?;
                let none_lit = self
                    .builder
                    .build_global_string_ptr("\"None\"", "list_opt_tup_none_lit")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                self.build_call(
                    strcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(none_wrap),
                        BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                    ],
                    "list_opt_none_cpy",
                )?;
                self.build_store(piece_slot, none_wrap)?;
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.builder.position_at_end(merge_bb);
                let piece = self
                    .build_load(
                        BasicTypeEnum::PointerType(i8_ptr),
                        piece_slot,
                        "list_opt_tup_piece_ld",
                    )?
                    .into_pointer_value();
                // Defined on every path; register so the sized helper's
                // per-iteration flush frees it exactly once.
                self.register_display_alloc(piece);
                Ok(piece)
            },
            "list_opt_tup_json",
        )
    }

    /// Serialize `List<(…)>` (ptrtoint slots) to a compact JSON array of arrays.
    pub(in crate::codegen) fn emit_list_product_tuple_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        elem_type_str: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let elem_ty = crate::codegen::extract_list_elem_type(&format!("List<{}>", elem_type_str))
            .unwrap_or_else(|| crate::ast::Type::Name("i32".into(), vec![]));
        let sty = match self.llvm_type_for(&elem_ty) {
            Some(BasicTypeEnum::StructType(s)) => s,
            _ => {
                return Err(CompileError::Generic(
                    "to_json List of tuple: cannot map element type".into(),
                ));
            }
        };
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_tup_json_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_tup_json_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        // D-4: exact-size two-pass assembly instead of the fixed 8192-byte
        // strcat loop. Elements are plain product tuples (no branching), so
        // each piece is defined unconditionally and the sized helper's flush
        // frees it exactly once per iteration (the old loop leaked every
        // element piece).
        self.emit_sized_list_of_pieces(
            len,
            ",",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_slot = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_tup_json_slot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let elem_i64 = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_tup_json_elem")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let elem_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_tup_json_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let loaded = self
                    .builder
                    .build_load(BasicTypeEnum::StructType(sty), elem_ptr, "list_tup_json_ld")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                // emit_product_tuple_to_json returns an unregistered buffer;
                // register so the sized helper's per-iteration flush frees it.
                let piece = self.emit_product_tuple_to_json(loaded)?;
                self.register_display_alloc(piece);
                Ok(piece)
            },
            "list_tup_json",
        )
    }

    /// 0.35.20 (#6): serialize a list struct value to a C string, dispatching
    /// on the full `List<...>` type name. Extracted from the print path so
    /// nested containers inside product tuples (e.g. `(List<i32>, List<i32>)`)
    /// can reuse the same element-kind dispatch.
    pub(super) fn emit_list_typed_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        list_ty: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let str_ptr = if list_ty == "List<string>" || list_ty.starts_with("List<string>") {
            self.emit_list_string_to_string(sv)?
        } else if list_ty.starts_with("List<List<")
            || list_ty
                .strip_prefix("List<")
                .is_some_and(|s| s.starts_with("List<"))
        {
            // Nested list: pick inner-list formatter from element type.
            let mid =
                Self::strip_first_type_arg(list_ty, "List").unwrap_or_else(|| "List".to_string());
            let elem = Self::strip_first_type_arg(&mid, "List").unwrap_or_default();
            if elem.starts_with('(') {
                // List of List of product tuples.
                self.emit_list_list_product_tuple_to_string(sv, &elem)?
            } else if elem == "f64" || elem == "f32" {
                self.emit_list_list_scalar_to_string(sv, ScalarListKind::F64)?
            } else if elem == "i64" {
                self.emit_list_list_scalar_to_string(sv, ScalarListKind::I64)?
            } else if elem == "bool" {
                self.emit_list_list_scalar_to_string(sv, ScalarListKind::Bool)?
            } else {
                let inner_fn = if elem == "string" {
                    "mimi_list_to_string"
                } else if elem.starts_with("Map") {
                    "mimi_list_map_to_string"
                } else if elem.starts_with("Set") {
                    "mimi_list_set_to_string"
                } else {
                    "mimi_list_i32_to_string"
                };
                self.emit_list_list_to_string(sv, inner_fn)?
            }
        } else if let Some(inner) = list_ty
            .strip_prefix("List<")
            .and_then(|s| s.strip_suffix('>'))
        {
            if self
                .type_defs
                .get(inner)
                .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
            {
                self.emit_list_record_to_string(sv, inner)?
            } else if inner.starts_with("Option") {
                self.emit_list_option_to_string(sv, inner)?
            } else if inner.starts_with("Result") {
                // Result of product uses uniform heap pack runtime.
                if let Some(ok_ty) = inner.strip_prefix("Result<").and_then(|s| {
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
                        let list_alloca = self.build_alloca(
                            BasicTypeEnum::StructType(self.list_struct_type()),
                            "list_res_prod_disp",
                        )?;
                        self.build_store(list_alloca, sv)?;
                        self.emit_list_result_product_runtime(list_alloca, &elem, 1)?
                    } else if ok_ty.starts_with("Map<string, ") {
                        if let Some(inner_val) = ok_ty
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
                                let list_alloca = self.build_alloca(
                                    BasicTypeEnum::StructType(self.list_struct_type()),
                                    "list_res_map_prod_disp",
                                )?;
                                self.build_store(list_alloca, sv)?;
                                self.emit_list_result_map_product_runtime(list_alloca, &elem, 1)?
                            } else {
                                self.emit_list_result_to_string(sv, inner)?
                            }
                        } else {
                            self.emit_list_result_to_string(sv, inner)?
                        }
                    } else if let Some(set_elem) =
                        ok_ty.strip_prefix("Set<").and_then(|s| s.strip_suffix('>'))
                    {
                        if set_elem.starts_with('(') || self.is_product_tuple_alias(set_elem) {
                            let elem = if self.is_product_tuple_alias(set_elem) {
                                self.resolve_alias_type_name(set_elem)
                            } else {
                                set_elem.to_string()
                            };
                            let list_alloca = self.build_alloca(
                                BasicTypeEnum::StructType(self.list_struct_type()),
                                "list_res_set_prod_disp",
                            )?;
                            self.build_store(list_alloca, sv)?;
                            self.emit_list_result_set_product_runtime(list_alloca, &elem, 1)?
                        } else {
                            self.emit_list_result_to_string(sv, inner)?
                        }
                    } else {
                        self.emit_list_result_to_string(sv, inner)?
                    }
                } else {
                    self.emit_list_result_to_string(sv, inner)?
                }
            } else if self
                .type_defs
                .get(inner)
                .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Enum(_)))
            {
                self.emit_list_enum_to_string(sv, inner)?
            } else if inner.starts_with("Map") {
                self.emit_list_map_to_string(sv, inner)?
            } else if inner.starts_with("Set") || inner == "set" {
                self.emit_list_set_to_string(sv, inner)?
            } else if inner.starts_with('(') || self.is_product_tuple_alias(inner) {
                // List of product tuples (or alias of them) as ptrtoint.
                let elem = if self.is_product_tuple_alias(inner) {
                    self.resolve_alias_type_name(inner)
                } else {
                    inner.to_string()
                };
                self.emit_list_product_tuple_to_string(sv, &elem)?
            } else if inner == "f64" || inner == "f32" {
                self.emit_list_scalar_to_string(sv, ScalarListKind::F64)?
            } else if inner == "i64" {
                self.emit_list_scalar_to_string(sv, ScalarListKind::I64)?
            } else if inner == "bool" {
                self.emit_list_scalar_to_string(sv, ScalarListKind::Bool)?
            } else {
                self.emit_list_i32_to_string(sv)?
            }
        } else {
            self.emit_list_i32_to_string(sv)?
        };
        Ok(str_ptr)
    }

    /// Format `List<(…)>` product tuples (ptrtoint slots) as `[(1, 2), …]`.
    fn emit_list_product_tuple_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        elem_type_str: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let elem_ty = crate::codegen::extract_list_elem_type(&format!("List<{}>", elem_type_str))
            .unwrap_or_else(|| crate::ast::Type::Name("i32".into(), vec![]));
        let sty = match self.llvm_type_for(&elem_ty) {
            Some(BasicTypeEnum::StructType(s)) => s,
            _ => {
                return self.emit_list_i32_to_string(sv);
            }
        };
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_tup_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        // Wave-1 audit fix (§8, FIX: fixed 4096-byte strcat assembly with no
        // capacity tracking): exact-size two-pass assembly. The element
        // renderer is pure, so the two passes render each tuple twice;
        // measurement pieces are freed per iteration via the sized helper.
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                let data_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 1, "list_tup_data_gep")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let data_ptr = self
                    .build_load(i8_ptr, data_gep, "list_tup_data")?
                    .into_pointer_value();
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // slots, loaded from the struct's data field). The sized
                // helper's loop guards `idx ULT len` gate every call.
                let elem_slot = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], "list_tup_slot")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let elem_i64 = self
                    .build_load(i64_ty, elem_slot, "list_tup_elem")?
                    .into_int_value();
                let elem_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_tup_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let loaded = self
                    .build_load(BasicTypeEnum::StructType(sty), elem_ptr, "list_tup_ld")?
                    .into_struct_value();
                self.emit_product_tuple_to_string(loaded, Some(elem_type_str))
            },
            "list_tup",
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Format `List<Enum>` as `[Red(), Blue(7), ...]`.
    fn emit_list_enum_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        enum_name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_enum_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        // D-4: exact-size two-pass assembly (fixed 4096-byte strcat removed).
        // The element renderer is pure (display formatter), so the two passes
        // render each element twice; measurement pieces are freed per
        // iteration by the sized helper.
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                let data_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 1, "list_enum_data_gep")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let data_ptr = self
                    .build_load(i8_ptr, data_gep, "list_enum_data")?
                    .into_pointer_value();
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // slots, loaded from the struct's data field). The sized
                // helper's loop guards `idx ULT len` gate every call.
                let elem_slot = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], "list_enum_slot")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let elem_i64 = self
                    .build_load(i64_ty, elem_slot, "list_enum_elem")?
                    .into_int_value();
                let enum_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_enum_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let enum_sty = self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(self.context.i32_type()),
                        BasicTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                let loaded = self
                    .builder
                    .build_load(
                        BasicTypeEnum::StructType(enum_sty),
                        enum_ptr,
                        "list_enum_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                self.emit_enum_display(enum_name, loaded)
            },
            "list_enum",
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Format `List<Result<…>>` as `[Ok(…), Err(…), ...]`.
    /// `elem_res_type` is the full Result type (e.g. `Result<Map<string, i32>, i32>`).
    fn emit_list_result_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        elem_res_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_res_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        // D-4: exact-size two-pass assembly (fixed 4096-byte strcat removed).
        // The element renderer is pure, so the two passes render each element
        // twice; measurement pieces are freed per iteration by the sized helper.
        let ok_rec = elem_res_type
            .strip_prefix("Result<")
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim())
            .filter(|inner| {
                !inner.is_empty()
                    && self
                        .type_defs
                        .get(*inner)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
            });
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                let data_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 1, "list_res_data_gep")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let data_ptr = self
                    .build_load(i8_ptr, data_gep, "list_res_data")?
                    .into_pointer_value();
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // slots, loaded from the struct's data field). The sized
                // helper's loop guards `idx ULT len` gate every call.
                let elem_slot = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], "list_res_slot")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let elem_i64 = self
                    .build_load(i64_ty, elem_slot, "list_res_elem")?
                    .into_int_value();
                let res_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_res_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                // Full Result layout (by-value Ok tuple/record) when known.
                let res_ty =
                    crate::codegen::extract_list_elem_type(&format!("List<{}>", elem_res_type))
                        .unwrap_or_else(|| {
                            crate::ast::Type::Name(
                                "Result".into(),
                                vec![
                                    crate::ast::Type::Name("i64".into(), vec![]),
                                    crate::ast::Type::Name("i64".into(), vec![]),
                                ],
                            )
                        });
                let res_sty = match self.llvm_type_for(&res_ty) {
                    Some(BasicTypeEnum::StructType(s)) => s,
                    _ => self.context.struct_type(
                        &[
                            BasicTypeEnum::IntType(self.context.bool_type()),
                            BasicTypeEnum::IntType(i64_ty),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    ),
                };
                let loaded = self
                    .builder
                    .build_load(BasicTypeEnum::StructType(res_sty), res_ptr, "list_res_ld")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                self.emit_result_to_string_typed(loaded, ok_rec, elem_res_type)
            },
            "list_res",
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Format `List<Option<…>>` as `[Some(…), None(), …]`.
    /// `elem_opt_type` is the full Option type string (e.g. `Option<Map<string, i32>>`).
    fn emit_list_option_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        elem_opt_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        // List elements for Option are typically by-value structs spilled as ptrtoint
        // of stack Option or packed; walk as i64 and interpret as Option via temp.
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_opt_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        // D-4: exact-size two-pass assembly (fixed 4096-byte strcat removed).
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                let data_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 1, "list_opt_data_gep")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let data_ptr = self
                    .build_load(i8_ptr, data_gep, "list_opt_data")?
                    .into_pointer_value();
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // slots, loaded from the struct's data field). The sized
                // helper's loop guards `idx ULT len` gate every call.
                let elem_slot = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], "list_opt_slot")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let elem_i64 = self
                    .build_load(i64_ty, elem_slot, "list_opt_elem")?
                    .into_int_value();
                let opt_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_opt_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                // Use full Option layout (by-value tuple / record payload) when
                // known; fall back to canonical {i1,i64} for scalar elements.
                let opt_ty =
                    crate::codegen::extract_list_elem_type(&format!("List<{}>", elem_opt_type))
                        .unwrap_or_else(|| {
                            crate::ast::Type::Name(
                                "Option".into(),
                                vec![crate::ast::Type::Name("i64".into(), vec![])],
                            )
                        });
                let opt_sty = match self.llvm_type_for(&opt_ty) {
                    Some(BasicTypeEnum::StructType(s)) => s,
                    _ => self.context.struct_type(
                        &[
                            BasicTypeEnum::IntType(self.context.bool_type()),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    ),
                };
                let loaded = self
                    .builder
                    .build_load(BasicTypeEnum::StructType(opt_sty), opt_ptr, "list_opt_ld")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                self.emit_option_to_string(loaded, None, elem_opt_type)
            },
            "list_opt",
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Format `List<Record>` as `[Point { ... }, ...]` matching interp Display.
    fn emit_list_record_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        record_name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_rec_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, alloca, 1, "list_rec_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_rec_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        // D-4: exact-size two-pass assembly instead of the fixed 4096-byte
        // strcat loop. Elements are record pointers (no branching), so each
        // piece is defined unconditionally and the sized helper's flush frees
        // it exactly once per iteration (the old loop leaked every element's
        // record display string). Display style: ", " separators.
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_rec_elem_ptr")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let elem_i64 = self
                    .builder
                    .build_load(i64_ty, elem_ptr, "list_rec_elem")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                // Treat as pointer to record struct.
                let rec_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_rec_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                // emit_record_display self-registers its final buffer (0.34.36
                // sized assembly); the sized helper's per-iteration flush
                // frees it — do not register again.
                let rec_str = self.emit_record_display(record_name, rec_ptr)?;
                Ok(rec_str)
            },
            "list_rec_display",
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Format custom enum `{i32 tag, i64 payload}` as `Variant` / `Variant(n)`.
    fn emit_enum_display(
        &self,
        type_name: &str,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let td = self.type_defs.get(type_name).ok_or_else(|| {
            CompileError::LlvmError(format!("no type def for enum {}", type_name))
        })?;
        let variants = match &td.kind {
            crate::ast::TypeDefKind::Enum(vs) => vs.clone(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "{} is not an enum",
                    type_name
                )))
            }
        };
        let mut sorted = variants;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let tag = self
            .build_extract_value(sv.into(), 0, "enum_tag")?
            .into_int_value();
        let payload = self
            .build_extract_value(sv.into(), 1, "enum_pay")?
            .into_int_value();
        let payload_i64 = if payload.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(payload, i64_ty, "enum_pay_i64")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        } else {
            payload
        };
        // §8-#96/D-4 residue (0.34.36): exact-size assembly per case arm —
        // the fixed 128-byte snprintf buffer silently truncated long string
        // payloads. Each arm wraps its payload into an UNREGISTERED buffer,
        // stores it in `out_slot`; the merge loads and registers it (defined
        // on every runtime path) so the consuming print's flush frees it
        // exactly once.
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let out_slot =
            self.build_alloca(BasicTypeEnum::PointerType(i8_ptr_ty), "enum_disp_slot")?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let merge_bb = self.context.append_basic_block(parent, "enum_disp_merge");
        let default_bb = self.context.append_basic_block(parent, "enum_disp_default");
        let mut switch_cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        let mut case_bbs: Vec<(usize, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();
        for (i, v) in sorted.iter().enumerate() {
            let case_bb = self
                .context
                .append_basic_block(parent, &format!("enum_disp_{}", v.name));
            switch_cases.push((tag.get_type().const_int(i as u64, false), case_bb));
            case_bbs.push((i, case_bb));
        }
        self.builder
            .build_switch(tag, default_bb, &switch_cases)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        for (i, case_bb) in case_bbs {
            self.builder.position_at_end(case_bb);
            let v = &sorted[i];
            let has_payload = v.payload.is_some();
            if has_payload {
                // String payloads are ptrtoint of {ptr,len}; decode when heapish.
                let is_str_payload = matches!(
                    &v.payload,
                    Some(crate::ast::VariantPayload::Tuple(ts))
                        if ts.len() == 1
                            && matches!(ts[0].unlocated(), crate::ast::Type::Name(n, _) if n == "string")
                );
                if is_str_payload {
                    let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                    let str_sty = self.context.struct_type(
                        &[
                            BasicTypeEnum::PointerType(i8_ptr),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    );
                    let as_ptr = self
                        .builder
                        .build_int_to_ptr(payload_i64, i8_ptr, &format!("enum_str_{}", v.name))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let loaded = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(str_sty),
                            as_ptr,
                            &format!("enum_str_ld_{}", v.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_struct_value();
                    let data_ptr = self
                        .build_extract_value(loaded.into(), 0, &format!("enum_data_{}", v.name))?
                        .into_pointer_value();
                    let prefix_g = self
                        .builder
                        .build_global_string_ptr(
                            &format!("{}(", v.name),
                            &format!("enum_spre_{}", v.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let wrap = self.emit_display_wrap_dyn(
                        prefix_g.as_pointer_value(),
                        data_ptr,
                        &format!("enum_wrap_s_{}", v.name),
                    )?;
                    self.build_store(out_slot, wrap)?;
                } else {
                    let arm_marker = self.display_marker();
                    let pay_str =
                        self.emit_display_i64_str(payload_i64, &format!("enum_pay_{}", v.name))?;
                    let prefix_g = self
                        .builder
                        .build_global_string_ptr(
                            &format!("{}(", v.name),
                            &format!("enum_pre_{}", v.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let wrap = self.emit_display_wrap_dyn(
                        prefix_g.as_pointer_value(),
                        pay_str,
                        &format!("enum_wrap_{}", v.name),
                    )?;
                    self.flush_display_since(arm_marker)?;
                    self.build_store(out_slot, wrap)?;
                }
            } else {
                let lit = self.emit_display_owned_copy(
                    format!("{}()", v.name),
                    &format!("enum_lit_{}", v.name),
                )?;
                self.build_store(out_slot, lit)?;
            }
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        }
        self.builder.position_at_end(default_bb);
        let unk = self.emit_display_lit_copy("Enum(?)", "enum_unk")?;
        self.build_store(out_slot, unk)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(merge_bb);
        let disp = self
            .build_load(
                BasicTypeEnum::PointerType(i8_ptr_ty),
                out_slot,
                "enum_disp_ld",
            )?
            .into_pointer_value();
        // Defined on every runtime path; the consuming print's
        // flush_display_frees releases it exactly once.
        self.register_display_alloc(disp);
        Ok(disp)
    }

    /// Format a named Record as `Name { field: value, ... }` (interp Display style).
    fn emit_record_display(
        &self,
        type_name: &str,
        struct_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let td = self
            .type_defs
            .get(type_name)
            .ok_or_else(|| CompileError::LlvmError(format!("no type def for {}", type_name)))?;
        let fields = match &td.kind {
            crate::ast::TypeDefKind::Record(fields) => fields.clone(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "{} is not a record",
                    type_name
                )))
            }
        };
        let llvm_ty = *self
            .type_llvm
            .get(type_name)
            .ok_or_else(|| CompileError::LlvmError(format!("no LLVM type for {}", type_name)))?;
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return Err(CompileError::LlvmError(format!(
                "{} is not a struct",
                type_name
            )));
        };
        let i64_ty = self.context.i64_type();
        // Sorted field names match interp Display (dual-stable).
        let mut idx_map: Vec<(usize, _)> = fields.iter().enumerate().collect();
        idx_map.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        // §8-#96/D-4 residue (0.34.36): exact-size part assembly — the old
        // est-size snprintf buffer silently truncated long string fields and
        // deeply nested records. Float fields now render via
        // mimi_to_string_f64 (shortest round-trip, VM-aligned) instead of %g.
        let rec_marker = self.display_marker();
        let mut parts: Vec<CatPart<'ctx>> = Vec::new();
        parts.push(CatPart::Owned(format!("{} {{ ", type_name)));
        for (pos, (i, field)) in idx_map.iter().enumerate() {
            if pos > 0 {
                parts.push(CatPart::Lit(", "));
            }
            let gep = self
                .gep()
                .build_struct_gep(sty, struct_ptr, *i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let ft = sty
                .get_field_type_at_index(*i as u32)
                .ok_or_else(|| CompileError::LlvmError("missing field".into()))?;
            let field_val = self.build_load(ft, gep, &format!("disp_{}", field.name))?;
            match field.ty.unlocated() {
                crate::ast::Type::Name(n, _) if n == "string" => {
                    let sv = field_val.into_struct_value();
                    let dp = self
                        .build_extract_value(sv.into(), 0, &format!("{}_p", field.name))?
                        .into_pointer_value();
                    parts.push(CatPart::Owned(format!("{}: ", field.name)));
                    parts.push(CatPart::Dyn(dp));
                }
                crate::ast::Type::Name(n, _) if matches!(n.as_str(), "i32" | "i64") => {
                    let iv = field_val.into_int_value();
                    let i64v = if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(iv, i64_ty, "disp_sext")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    };
                    let s = self.emit_display_i64_str(i64v, &format!("disp_i_{}", field.name))?;
                    parts.push(CatPart::Owned(format!("{}: ", field.name)));
                    parts.push(CatPart::Dyn(s));
                }
                crate::ast::Type::Name(n, _) if n == "bool" => {
                    let iv = field_val.into_int_value();
                    let true_g = self
                        .builder
                        .build_global_string_ptr("true", &format!("{}_t", field.name))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let false_g = self
                        .builder
                        .build_global_string_ptr("false", &format!("{}_f", field.name))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let zero = iv.get_type().const_int(0, false);
                    let is_t = self
                        .builder
                        .build_int_compare(IntPredicate::NE, iv, zero, "disp_b")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let sel = self
                        .builder
                        .build_select(
                            is_t,
                            true_g.as_pointer_value(),
                            false_g.as_pointer_value(),
                            "disp_bs",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    parts.push(CatPart::Owned(format!("{}: ", field.name)));
                    parts.push(CatPart::Dyn(sel.into_pointer_value()));
                }
                crate::ast::Type::Name(n, _) if n == "f64" => {
                    let s = self.emit_display_f64_str(
                        field_val.into_float_value(),
                        &format!("disp_f_{}", field.name),
                    )?;
                    parts.push(CatPart::Owned(format!("{}: ", field.name)));
                    parts.push(CatPart::Dyn(s));
                }
                crate::ast::Type::Name(n, _)
                    if self.type_defs.get(n).is_some_and(|td| {
                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                    }) =>
                {
                    // Nested named record: recursive Display string.
                    let nested_ptr = match field_val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        BasicValueEnum::StructValue(sv) => {
                            let nested_ty = *self.type_llvm.get(n).ok_or_else(|| {
                                CompileError::LlvmError(format!("no LLVM type for {}", n))
                            })?;
                            let BasicTypeEnum::StructType(nsty) = nested_ty else {
                                return Err(CompileError::LlvmError(format!(
                                    "{} is not a struct",
                                    n
                                )));
                            };
                            let alloca = self.build_alloca(
                                BasicTypeEnum::StructType(nsty),
                                &format!("nest_{}", field.name),
                            )?;
                            self.build_store(alloca, sv)?;
                            alloca
                        }
                        _ => {
                            return Err(CompileError::LlvmError(format!(
                                "nested record field '{}' unexpected kind",
                                field.name
                            )))
                        }
                    };
                    let nested_str = self.emit_record_display(n, nested_ptr)?;
                    parts.push(CatPart::Owned(format!("{}: ", field.name)));
                    parts.push(CatPart::Dyn(nested_str));
                }
                _ => {
                    parts.push(CatPart::Owned(format!("{}: ?", field.name)));
                }
            }
        }
        parts.push(CatPart::Lit(" }"));
        let buf = self.sized_cat_parts(&parts, "rec_disp", false)?;
        // Scratch (i64/f64 renders, nested record results) was consumed by
        // the assembly memcpy above; release it now, then register the final
        // buffer so the consuming print's flush frees it exactly once.
        self.flush_display_since(rec_marker)?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Wave-1 audit fix (§8, FIX: Map/Set display routing by substring
    /// `contains`): extract the value type of the FIRST `Map<string, V>`
    /// occurrence in `type_name` with bracket-depth tracking (same scan the
    /// existing `map_nested_product_mode` uses). Substring matching
    /// false-positived on nested types — e.g. `Map<string, Map<string,
    /// string>>` matched the INNER map's pattern and misrouted the outer
    /// handle through `mimi_map_to_json_string`.
    fn map_value_type_of(type_name: &str) -> Option<String> {
        let idx = type_name.find("Map<string,")?;
        let rest = &type_name[idx + "Map<string,".len()..];
        let mut depth = 0i32;
        for (j, ch) in rest.char_indices() {
            match ch {
                '<' | '(' => depth += 1,
                '>' | ')' if depth == 0 => return Some(rest[..j].trim().to_string()),
                '>' | ')' => depth -= 1,
                _ => {}
            }
        }
        None
    }

    /// Wave-1 audit fix (§8): element-type extraction for `Set<…>` —
    /// bracket-depth aware; see `map_value_type_of`.
    fn set_elem_type_of(type_name: &str) -> Option<String> {
        let idx = type_name.find("Set<")?;
        let rest = &type_name[idx + "Set<".len()..];
        let mut depth = 0i32;
        for (j, ch) in rest.char_indices() {
            match ch {
                '<' | '(' => depth += 1,
                '>' | ')' if depth == 0 => return Some(rest[..j].trim().to_string()),
                '>' | ')' => depth -= 1,
                _ => {}
            }
        }
        None
    }

    /// Pick map JSON runtime helper from a type string containing `Map<…>`.
    /// Routes on the parsed map value type (full outer match of V), NOT on
    /// substring `contains` (Wave-1 audit fix §8).
    fn map_json_fn_for_type(type_name: &str) -> &'static str {
        match Self::map_value_type_of(type_name).as_deref() {
            Some("string") => "mimi_map_to_json_string",
            Some("bool") => "mimi_map_to_json_bool",
            Some("f64") | Some("f32") => "mimi_map_to_json_f64",
            _ => "mimi_map_to_json_i64",
        }
    }

    /// Mode for option/result map JSON helpers:
    /// 0-3 scalar; 10+arity product; 20+ List product; 30+ Set; 40+ Map of Map.
    pub(in crate::codegen) fn map_nested_product_mode(&self, map_type: &str) -> i64 {
        let val_ty = map_type
            .find("Map<string,")
            .map(|i| &map_type[i + "Map<string,".len()..])
            .map(|s| s.trim_start())
            .and_then(|s| {
                let mut depth = 0i32;
                for (j, ch) in s.char_indices() {
                    match ch {
                        '<' | '(' => depth += 1,
                        '>' if depth == 0 => return Some(s[..j].trim()),
                        '>' | ')' => depth -= 1,
                        _ => {}
                    }
                }
                None
            });
        let Some(val_ty) = val_ty else {
            return 0;
        };
        let product_arity = |prod: &str| -> i64 {
            let body = prod
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(prod);
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
        if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
            let elem = if self.is_product_tuple_alias(val_ty) {
                self.resolve_alias_type_name(val_ty)
            } else {
                val_ty.to_string()
            };
            return 10 + product_arity(&elem);
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
                return 50 + product_arity(&elem);
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
                    return 60 + product_arity(&elem);
                }
            }
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
                return 20 + product_arity(&elem);
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
                return 30 + product_arity(&elem);
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
                    return 40 + product_arity(&elem);
                }
            }
        }
        0
    }

    /// Pick set Display runtime helper from a type string containing `Set<…>`.
    /// Routes on the parsed element type (full outer match of E), NOT on
    /// substring `contains` (Wave-1 audit fix §8).
    fn set_display_fn_for_type(type_name: &str) -> &'static str {
        match Self::set_elem_type_of(type_name).as_deref() {
            Some("string") => "mimi_set_to_display_string",
            Some("bool") => "mimi_set_to_display_bool",
            Some("f64") | Some("f32") => "mimi_set_to_display_f64",
            _ => "mimi_set_to_display",
        }
    }

    /// Strip first type argument from `Prefix<A, …>` / `Prefix<A>` → `A`.
    /// Handles nested brackets (e.g. `Result<Option<Map<string, i32>>, i32>`).
    pub(in crate::codegen) fn strip_first_type_arg(
        type_name: &str,
        prefix: &str,
    ) -> Option<String> {
        let rest = type_name.strip_prefix(prefix)?.strip_prefix('<')?;
        // Track both angle-bracket and paren depth so product tuples like
        // List<(i32, i32)> do not split on the comma inside the tuple.
        let mut depth = 0i32;
        for (i, ch) in rest.char_indices() {
            match ch {
                '<' | '(' => depth += 1,
                '>' | ')' => {
                    depth -= 1;
                    if depth < 0 {
                        return Some(rest[..i].trim().to_string());
                    }
                }
                ',' if depth == 0 => return Some(rest[..i].trim().to_string()),
                _ => {}
            }
        }
        None
    }

    /// Emit JSON for a Result Err payload: string Err is ptrtoint of heap
    /// `{ptr,len}`; scalar Err is a plain i64. Uses the same ≥1MB + 8-aligned
    /// heapish heuristic as Display so we do not depend on type-name strings.
    pub(in crate::codegen) fn emit_result_err_json(
        &self,
        err_i64: inkwell::values::IntValue<'ctx>,
        prefer_string: bool,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let snprintf_fn = self.get_runtime_fn("snprintf")?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let out_slot = self.build_alloca(BasicTypeEnum::PointerType(i8_ptr), "res_err_j_out")?;

        // Heapish pointer? (≥1MB and 8-byte aligned) → decode as string struct.
        let min_heap = i64_ty.const_int(1_048_576, false);
        let ge = self
            .builder
            .build_int_compare(IntPredicate::UGE, err_i64, min_heap, "res_err_ge")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let and7 = self
            .builder
            .build_and(err_i64, i64_ty.const_int(7, false), "res_err_and7")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let aligned = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                and7,
                i64_ty.const_int(0, false),
                "res_err_al",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let is_heapish = self
            .builder
            .build_and(ge, aligned, "res_err_heapish")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        if prefer_string {
            // Type says string Err: still require alignment, but allow lower addresses
            // only when heapish already; prefer_string only affects non-heapish fallback
            // path (still print as number if not a pointer).
            let _ = prefer_string;
        }

        let str_bb = self.context.append_basic_block(parent, "res_err_j_str");
        let int_bb = self.context.append_basic_block(parent, "res_err_j_int");
        let merge_bb = self.context.append_basic_block(parent, "res_err_j_merge");
        self.builder
            .build_conditional_branch(is_heapish, str_bb, int_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;

        self.builder.position_at_end(str_bb);
        {
            let str_sty = self.context.struct_type(
                &[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            );
            let as_ptr = self
                .builder
                .build_int_to_ptr(err_i64, i8_ptr, "res_err_str_ptr")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let loaded = self
                .builder
                .build_load(BasicTypeEnum::StructType(str_sty), as_ptr, "res_err_str_ld")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                .into_struct_value();
            let data_ptr = self
                .build_extract_value(loaded.into(), 0, "res_err_data")?
                .into_pointer_value();
            let escape_fn = self.get_runtime_fn("mimi_json_escape_string")?;
            let escaped = self
                .build_call(
                    escape_fn,
                    &[BasicMetadataValueEnum::PointerValue(data_ptr)],
                    "res_err_escaped",
                )?
                .try_as_basic_value_opt()
                .ok_or("mimi_json_escape_string void")?
                .into_pointer_value();
            // Wave-1 audit fix (§8): sized wrap instead of fixed 1024 snprintf
            // (long Err strings truncated / would overflow). `escaped` is copied
            // synchronously, so it stays freed right after as before.
            let wrap = self.sized_cat_parts(
                &[
                    CatPart::Lit("{\"Err\":["),
                    CatPart::Dyn(escaped),
                    CatPart::Lit("]}"),
                ],
                "res_err_json_wrap",
                false,
            )?;
            if let Ok(free_fn) = self.get_runtime_fn("free") {
                self.build_call(
                    free_fn,
                    &[BasicMetadataValueEnum::PointerValue(escaped)],
                    "res_err_free_esc",
                )?;
            }
            self.build_store(out_slot, wrap)?;
        }
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;

        self.builder.position_at_end(int_bb);
        {
            let wrap = self.malloc_or_abort(i64_ty.const_int(64, false), "res_err_num_wrap")?;
            let fmt = self
                .builder
                .build_global_string_ptr("{\"Err\":[%ld]}", "res_err_num_fmt")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            self.build_call(
                snprintf_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(wrap),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(64, false)),
                    BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                    BasicMetadataValueEnum::IntValue(err_i64),
                ],
                "res_err_num_sn",
            )?;
            self.build_store(out_slot, wrap)?;
        }
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;

        self.builder.position_at_end(merge_bb);
        Ok(self
            .build_load(BasicTypeEnum::PointerType(i8_ptr), out_slot, "res_err_j_ld")?
            .into_pointer_value())
    }

    /// Convenience: always use heapish heuristic for Err JSON.
    #[allow(dead_code)]
    pub(in crate::codegen) fn emit_result_err_string_json(
        &self,
        err_i64: inkwell::values::IntValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_result_err_json(err_i64, true)
    }

    /// Serialize a `{ ptr, len }` heap string payload (String value) into a
    /// JSON string literal (`"..."`, quoted + escaped via
    /// `mimi_json_escape_string`). Matches the JSON VM's `Value::String`
    /// branch of `to_json`. Used by the native emitter for Option/Result
    /// string payloads, which previously hit the generic "unexpected
    /// StructType" rejection (D-3 resolved-gap fix).
    ///
    /// Returns the heap-allocated JSON C-string (caller owns/frees it).
    pub(in crate::codegen) fn emit_heap_string_payload_json(
        &self,
        payload: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let ptr = self
            .build_extract_value(payload.into(), 0, "str_payload_ptr")?
            .into_pointer_value();
        let escape_fn = self.get_runtime_fn("mimi_json_escape_string")?;
        let escaped = self
            .build_call(
                escape_fn,
                &[BasicMetadataValueEnum::PointerValue(ptr)],
                "str_payload_escaped",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_json_escape_string void")?
            .into_pointer_value();
        // Upstream caller registers/frees `escaped` as appropriate.
        Ok(escaped)
    }

    /// D-3: emit `{"Some":[<json>]}` / `{"None"}` for an Option whose payload
    /// is a heap string. `payload_i64` is the ptrtoint of the escaped JSON
    /// string literal produced by `emit_heap_string_payload_json`; cast back
    /// and embed with sized assembly (no fixed-buffer truncation).
    pub(in crate::codegen) fn emit_option_string_to_json_cstr(
        &self,
        disc_i64: inkwell::values::IntValue<'ctx>,
        payload_i64: inkwell::values::IntValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let disc_is_some = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                disc_i64,
                i64_ty.const_int(0, false),
                "opt_str_some",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let some_bb = self.context.append_basic_block(parent, "opt_str_some_bb");
        let none_bb = self.context.append_basic_block(parent, "opt_str_none_bb");
        let merge_bb = self.context.append_basic_block(parent, "opt_str_merge_bb");
        let out_slot = self.build_alloca(BasicTypeEnum::PointerType(i8_ptr), "opt_str_out")?;
        self.builder
            .build_conditional_branch(disc_is_some, some_bb, none_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;

        self.builder.position_at_end(some_bb);
        {
            let json_ptr = self
                .builder
                .build_int_to_ptr(payload_i64, i8_ptr, "opt_str_json_ptr")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let wrap = self.sized_cat_parts(
                &[
                    CatPart::Lit("{\"Some\":["),
                    CatPart::Dyn(json_ptr),
                    CatPart::Lit("]}"),
                ],
                "opt_str_wrap",
                false,
            )?;
            self.build_store(out_slot, wrap)?;
        }
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;

        self.builder.position_at_end(none_bb);
        {
            let none_heap =
                self.malloc_or_abort(i64_ty.const_int(8, false), "opt_str_none_heap")?;
            let none_lit = self
                .builder
                .build_global_string_ptr("\"None\"", "opt_str_none_lit")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let strcpy_fn = self.get_runtime_fn("strcpy")?;
            self.build_call(
                strcpy_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(none_heap),
                    BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                ],
                "opt_str_none_cpy",
            )?;
            self.build_store(out_slot, none_heap)?;
        }
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;

        self.builder.position_at_end(merge_bb);
        Ok(self
            .build_load(
                BasicTypeEnum::PointerType(i8_ptr),
                out_slot,
                "opt_str_result",
            )?
            .into_pointer_value())
    }

    /// Serialize a by-value Result struct `{i1, ok, err}` to a JSON C string.
    /// Handles product-tuple / record Ok and string Err.
    pub(in crate::codegen) fn emit_result_struct_to_json_cstr(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        res_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let disc = self
            .build_extract_value(sv.into(), 0, "res_j_disc")?
            .into_int_value();
        let is_ok = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                disc,
                disc.get_type().const_int(0, false),
                "res_j_is_ok",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let ok_bb = self.context.append_basic_block(parent, "res_j_ok");
        let err_bb = self.context.append_basic_block(parent, "res_j_err");
        let merge_bb = self.context.append_basic_block(parent, "res_j_merge");
        let out_slot = self.build_alloca(BasicTypeEnum::PointerType(i8_ptr), "res_j_out")?;
        self.builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(ok_bb);
        let ok_pay = self.build_extract_value(sv.into(), 1, "res_j_ok")?;
        let ok_inner = Self::strip_first_type_arg(res_type, "Result").unwrap_or_default();
        let is_named_record = self
            .type_defs
            .get(&ok_inner)
            .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)));
        let ok_json = if let BasicValueEnum::StructValue(ok_sv) = ok_pay {
            let ofields = ok_sv.get_type().get_field_types();
            let ok_is_nested_result = ofields.len() >= 3
                && matches!(
                    ofields[0],
                    BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                );
            let ok_is_list = ofields.len() == 2
                && matches!(
                    ofields[0],
                    BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                )
                && matches!(ofields[1], BasicTypeEnum::PointerType(_));
            let ok_is_string = ofields.len() == 2
                && matches!(ofields[0], BasicTypeEnum::PointerType(_))
                && matches!(ofields[1], BasicTypeEnum::IntType(t) if t.get_bit_width() == 64);
            if ok_is_string {
                // D-3: heap-string Ok payload {ptr,i64} is NOT a 2-field
                // product tuple — emit a JSON string literal instead of
                // the generic tuple path's [ptr,len] mis-serialization.
                self.emit_heap_string_payload_json(ok_sv)?
            } else if ok_is_nested_result {
                self.emit_result_struct_to_json_cstr(ok_sv, &ok_inner)?
            } else if ok_is_list || ok_inner.starts_with("List") {
                let tmp = self.build_alloca(
                    BasicTypeEnum::StructType(ok_sv.get_type()),
                    "res_j_list_tmp",
                )?;
                self.build_store(tmp, ok_sv)?;
                self.emit_list_payload_to_json_cstr(tmp, &ok_inner)?
            } else if is_named_record {
                let rec_ty = ok_sv.get_type();
                let rec_alloca =
                    self.build_alloca(BasicTypeEnum::StructType(rec_ty), "res_j_rec")?;
                self.build_store(rec_alloca, ok_sv)?;
                self.compile_record_to_json_cstr(&ok_inner, rec_alloca)?
            } else {
                self.emit_product_tuple_to_json(ok_sv)?
            }
        } else if let BasicValueEnum::PointerValue(pv) = ok_pay {
            // Result Ok of List often stores a pointer to the list struct.
            if ok_inner.starts_with("List") {
                self.emit_list_payload_to_json_cstr(pv, &ok_inner)?
            } else {
                let as_i64 = self.build_ptr_to_int(pv, i64_ty, "res_j_ok_ptr_i64")?;
                let tmp = self.malloc_or_abort(i64_ty.const_int(64, false), "res_j_ok_tmp")?;
                let ifmt = self
                    .builder
                    .build_global_string_ptr("%ld", "res_j_ok_ifmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(tmp),
                        BasicMetadataValueEnum::IntValue(i64_ty.const_int(64, false)),
                        BasicMetadataValueEnum::PointerValue(ifmt.as_pointer_value()),
                        BasicMetadataValueEnum::IntValue(as_i64),
                    ],
                    "res_j_ok_sn",
                )?;
                tmp
            }
        } else {
            // Scalar Ok.
            let ok_i64 = match ok_pay {
                BasicValueEnum::IntValue(iv) => {
                    if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(iv, i64_ty, "res_j_ok_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    }
                }
                _ => i64_ty.const_int(0, false),
            };
            if ok_inner.starts_with("List") {
                let list_ptr = self
                    .builder
                    .build_int_to_ptr(
                        ok_i64,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "res_j_list_from_i64",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.emit_list_payload_to_json_cstr(list_ptr, &ok_inner)?
            } else {
                let tmp = self.malloc_or_abort(i64_ty.const_int(64, false), "res_j_ok_tmp")?;
                let ifmt = self
                    .builder
                    .build_global_string_ptr("%ld", "res_j_ok_ifmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(tmp),
                        BasicMetadataValueEnum::IntValue(i64_ty.const_int(64, false)),
                        BasicMetadataValueEnum::PointerValue(ifmt.as_pointer_value()),
                        BasicMetadataValueEnum::IntValue(ok_i64),
                    ],
                    "res_j_ok_sn",
                )?;
                tmp
            }
        };
        // Wave-1 audit fix (§8): sized wrap instead of fixed 1024 snprintf.
        let wrap = self.sized_cat_parts(
            &[
                CatPart::Lit("{\"Ok\":["),
                CatPart::Dyn(ok_json),
                CatPart::Lit("]}"),
            ],
            "res_j_ok_wrap",
            false,
        )?;
        self.build_store(out_slot, wrap)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(err_bb);
        let err_pay = self.build_extract_value(sv.into(), 2, "res_j_err")?;
        let err_i64 = match err_pay {
            BasicValueEnum::IntValue(iv) => {
                if iv.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(iv, i64_ty, "res_j_err_i64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    iv
                }
            }
            _ => i64_ty.const_int(0, false),
        };
        let ewrap = self.emit_result_err_json(err_i64, true)?;
        self.build_store(out_slot, ewrap)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(merge_bb);
        Ok(self
            .build_load(BasicTypeEnum::PointerType(i8_ptr), out_slot, "res_j_result")?
            .into_pointer_value())
    }

    /// Format Result {i1, ok, err} as `Ok(...)` / `Err(...)` (int, string, or record Ok).
    fn emit_result_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        ok_record: Option<&str>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_result_to_string_typed(sv, ok_record, "")
    }

    fn emit_result_to_string_typed(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        ok_record: Option<&str>,
        arg_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let fields = sv.get_type().get_field_types();
        let disc = self
            .build_extract_value(sv.into(), 0, "res_disc")?
            .into_int_value();
        let ok_val = self.build_extract_value(sv.into(), 1, "res_ok")?;
        let err_val = self.build_extract_value(sv.into(), 2, "res_err")?;
        // §8-#96/D-4 residue (0.34.36): exact-size assembly per arm — the
        // fixed 256-byte snprintf("Ok(%s)"/"Err(%s)") buffer silently
        // truncated long payloads. Each arm wraps its rendered payload into
        // an UNREGISTERED buffer and stores it in `out_slot`; the merge
        // loads and registers it (defined on every runtime path) so the
        // consuming print's flush frees it exactly once. Arm scratch (nested
        // Display strings, i64/f64 renders) stays registered and is released
        // by the arm-end flush after the wrap's memcpy consumed it.
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let out_slot =
            self.build_alloca(BasicTypeEnum::PointerType(i8_ptr_ty), "res_print_slot")?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent fn".into()))?;
        let ok_bb = self.context.append_basic_block(parent, "res_print_ok");
        let err_bb = self.context.append_basic_block(parent, "res_print_err");
        let merge_bb = self.context.append_basic_block(parent, "res_print_merge");
        let zero = disc.get_type().const_int(0, false);
        let is_ok = self
            .builder
            .build_int_compare(IntPredicate::NE, disc, zero, "res_is_ok")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;

        let emit_arm = |label: &str,
                        bb: inkwell::basic_block::BasicBlock<'ctx>,
                        val: BasicValueEnum<'ctx>,
                        field_ty: BasicTypeEnum<'ctx>|
         -> MimiResult<()> {
            self.builder.position_at_end(bb);
            let wrap_prefix: &'static str = if label == "ok" { "Ok(" } else { "Err(" };
            // Arm-local marker: the arm-end flush frees exactly this arm's
            // scratch. A shared pre-branch marker would (a) let the ok arm's
            // flush free the err arm's compile-time registrations (runtime
            // free(undef)) and (b) free the merge-registered wrap of THIS
            // arm (use-after-free). The wrap is unregistered when stored, so
            // it survives; the merge registers it after both arms compiled.
            let arm_marker = self.display_marker();
            // Ok arm with named record payload (by-value, pointer, or ptrtoint).
            if label == "ok" {
                if let Some(rec_name) = ok_record {
                    let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                    let rec_ptr = match val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        BasicValueEnum::IntValue(iv) => self
                            .builder
                            .build_int_to_ptr(iv, i8_ptr, "res_ok_rec")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                        BasicValueEnum::StructValue(sv) => {
                            // By-value record in Result Ok slot.
                            let tmp = self.build_alloca(
                                BasicTypeEnum::StructType(sv.get_type()),
                                "res_ok_rec_tmp",
                            )?;
                            self.build_store(tmp, sv)?;
                            tmp
                        }
                        _ => {
                            return Err(CompileError::LlvmError(
                                "Result Ok record payload unexpected kind".into(),
                            ))
                        }
                    };
                    let rec_str = self.emit_record_display(rec_name, rec_ptr)?;
                    let wrap = self.emit_display_wrap("Ok(", rec_str, "res_ok_rec_wrap")?;
                    self.build_store(out_slot, wrap)?;
                    self.flush_display_since(arm_marker)?;
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    return Ok(());
                }
                // Deep-eval 2026-08-09 (09_io_files parity): `Ok(())` — the
                // unit payload lowers to i64464 zero; display it as `()` to
                // match the interpreter instead of `0`.
                if matches!(val, BasicValueEnum::IntValue(_)) {
                    let ok_root =
                        Self::strip_first_type_arg(arg_type, "Result").unwrap_or_default();
                    if ok_root == "()" || ok_root == "unit" {
                        let unit_str = self
                            .builder
                            .build_global_string_ptr("()", "res_ok_unit")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let wrap = self.emit_display_wrap(
                            "Ok(",
                            unit_str.as_pointer_value(),
                            "res_ok_unit_wrap",
                        )?;
                        self.build_store(out_slot, wrap)?;
                        self.flush_display_since(arm_marker)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        return Ok(());
                    }
                }
            }
            match field_ty {
                BasicTypeEnum::PointerType(_) if label == "ok" => {
                    let ptr = val.into_pointer_value();
                    // Result of List: pointer to list struct.
                    let list_ty = self.list_struct_type();
                    let loaded = self
                        .builder
                        .build_load(BasicTypeEnum::StructType(list_ty), ptr, "res_ok_list_ld")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_struct_value();
                    let list_str = self.emit_result_ok_list_display(loaded, arg_type)?;
                    let wrap = self.emit_display_wrap("Ok(", list_str, "res_ok_list_wrap")?;
                    self.build_store(out_slot, wrap)?;
                }
                BasicTypeEnum::StructType(sty)
                    if label == "ok"
                        && sty.get_field_types().len() == 2
                        && matches!(
                            sty.get_field_types()[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        )
                        && matches!(sty.get_field_types()[1], BasicTypeEnum::PointerType(_)) =>
                {
                    // Nested List by-value in Result Ok: {i64, ptr}.
                    let list_str =
                        self.emit_result_ok_list_display(val.into_struct_value(), arg_type)?;
                    let wrap = self.emit_display_wrap("Ok(", list_str, "res_ok_list_sv_wrap")?;
                    self.build_store(out_slot, wrap)?;
                }
                BasicTypeEnum::StructType(sty)
                    if label == "ok"
                        && sty.get_field_types().len() >= 3
                        && matches!(
                            sty.get_field_types()[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        ) =>
                {
                    // Nested Result by-value in Ok: Result<Result<…>,…>.
                    let nested_ty = Self::strip_first_type_arg(arg_type, "Result")
                        .unwrap_or_else(|| "Result".to_string());
                    let nested = self.emit_result_to_string_typed(
                        val.into_struct_value(),
                        None,
                        &nested_ty,
                    )?;
                    let wrap = self.emit_display_wrap("Ok(", nested, "res_ok_nested_wrap")?;
                    self.build_store(out_slot, wrap)?;
                }
                BasicTypeEnum::StructType(sty)
                    if label == "ok"
                        && sty.get_field_types().len() == 2
                        && matches!(sty.get_field_types()[0], BasicTypeEnum::PointerType(_))
                        && matches!(
                            sty.get_field_types()[1],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        ) =>
                {
                    // Mimi string {ptr, len} by-value in Result Ok — print data as %s.
                    let sv = val.into_struct_value();
                    let data_ptr = self
                        .build_extract_value(sv.into(), 0, "res_ok_str_ptr")?
                        .into_pointer_value();
                    // Legacy string data is NUL-terminated (runtime allocs
                    // reserve len+1); the len slot is not reliably
                    // maintained on every path, so strlen is the contract.
                    let wrap = self.emit_display_wrap("Ok(", data_ptr, "res_ok_str_wrap")?;
                    self.build_store(out_slot, wrap)?;
                }
                BasicTypeEnum::StructType(sty)
                    if label == "ok"
                        && sty.get_field_types().len() >= 2
                        && !matches!(
                            sty.get_field_types()[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        )
                        && !(matches!(sty.get_field_types()[0], BasicTypeEnum::PointerType(_))
                            && sty.get_field_types().len() == 2
                            && matches!(
                                sty.get_field_types()[1],
                                BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                            )) =>
                {
                    // Product tuple by-value in Result Ok: e.g. (i32,i32).
                    // Skip Mimi string {ptr,len} (handled above).
                    // Custom enums {i32 tag, i64 payload} handled below if needed.
                    let is_enum_layout = sty.get_field_types().len() == 2
                        && matches!(
                            sty.get_field_types()[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 32
                        )
                        && matches!(
                            sty.get_field_types()[1],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        );
                    if is_enum_layout {
                        // Result Ok of custom enum — use enum display when type known.
                        let ok_ty =
                            Self::strip_first_type_arg(arg_type, "Result").unwrap_or_default();
                        if !ok_ty.is_empty()
                            && self.type_defs.get(&ok_ty).is_some_and(|td| {
                                matches!(td.kind, crate::ast::TypeDefKind::Enum(_))
                            })
                        {
                            let enum_str =
                                self.emit_enum_display(&ok_ty, val.into_struct_value())?;
                            let wrap =
                                self.emit_display_wrap("Ok(", enum_str, "res_ok_enum_wrap")?;
                            self.build_store(out_slot, wrap)?;
                        } else {
                            let tup_str = self.emit_product_tuple_to_string(
                                val.into_struct_value(),
                                Some(&ok_ty),
                            )?;
                            let wrap = self.emit_display_wrap("Ok(", tup_str, "res_ok_tup_wrap")?;
                            self.build_store(out_slot, wrap)?;
                        }
                    } else {
                        let ok_ty2 = Self::strip_first_type_arg(arg_type, "Result");
                        let tup_str = self.emit_product_tuple_to_string(
                            val.into_struct_value(),
                            ok_ty2.as_deref(),
                        )?;
                        let wrap = self.emit_display_wrap("Ok(", tup_str, "res_ok_tup_wrap")?;
                        self.build_store(out_slot, wrap)?;
                    }
                }
                BasicTypeEnum::IntType(_) => {
                    let iv = val.into_int_value();
                    let as_i64 = if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(iv, i64_ty, &format!("{}_i64", label))
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    };
                    // Result of Map/Set: Ok payload is opaque handle (i64).
                    // Prefer Set root before Map contains — Result<Set<Map<…>>>
                    // must not take the Map branch (nested Map substring).
                    if label == "ok" && (arg_type.contains("Map<") || arg_type.contains("Set<")) {
                        let ok_root = arg_type
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
                            .unwrap_or(arg_type);
                        let disp = if ok_root.starts_with("Set<") {
                            // force Set branch via empty Map path skip
                            // handled below in else branch for Set
                            // Use a marker: fall through by not matching Map root
                            // restructure: Set first
                            let set_inner = ok_root;
                            if let Some(elem) = set_inner
                                .strip_prefix("Set<")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                                    let resolved = if self.is_product_tuple_alias(elem) {
                                        self.resolve_alias_type_name(elem)
                                    } else {
                                        elem.to_string()
                                    };
                                    self.emit_set_product_to_json(as_i64, &resolved, 1)?
                                } else if elem.starts_with("Map<string, ") {
                                    if let Some(val_ty) = elem
                                        .strip_prefix("Map<string, ")
                                        .and_then(|s| s.strip_suffix('>'))
                                    {
                                        if val_ty.starts_with('(')
                                            || self.is_product_tuple_alias(val_ty)
                                        {
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
                                                "mimi_set_to_json_map_product_i64",
                                            )?;
                                            let i64_ty = self.context.i64_type();
                                            self.build_call(
                                                func,
                                                &[
                                                    BasicMetadataValueEnum::IntValue(as_i64),
                                                    BasicMetadataValueEnum::IntValue(
                                                        i64_ty.const_int(arity as u64, false),
                                                    ),
                                                    BasicMetadataValueEnum::IntValue(
                                                        i64_ty.const_int(1, false),
                                                    ),
                                                ],
                                                "result_set_map_disp",
                                            )?
                                            .try_as_basic_value_opt()
                                            .ok_or("result set map display void")?
                                            .into_pointer_value()
                                        } else {
                                            let set_fn =
                                                self.get_runtime_fn("mimi_set_to_display")?;
                                            self.build_call(
                                                set_fn,
                                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                                "result_set_disp",
                                            )?
                                            .try_as_basic_value_opt()
                                            .ok_or("set display void")?
                                            .into_pointer_value()
                                        }
                                    } else {
                                        let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                                        self.build_call(
                                            set_fn,
                                            &[BasicMetadataValueEnum::IntValue(as_i64)],
                                            "result_set_disp",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("set display void")?
                                        .into_pointer_value()
                                    }
                                } else if let Some(opt_inner) = elem
                                    .strip_prefix("Option<")
                                    .and_then(|s| s.strip_suffix('>'))
                                {
                                    if opt_inner.starts_with('(')
                                        || self.is_product_tuple_alias(opt_inner)
                                    {
                                        let resolved = if self.is_product_tuple_alias(opt_inner) {
                                            self.resolve_alias_type_name(opt_inner)
                                        } else {
                                            opt_inner.to_string()
                                        };
                                        self.emit_set_option_product_to_json(as_i64, &resolved, 1)?
                                    } else {
                                        let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                                        self.build_call(
                                            set_fn,
                                            &[BasicMetadataValueEnum::IntValue(as_i64)],
                                            "result_set_disp",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("set display void")?
                                        .into_pointer_value()
                                    }
                                } else {
                                    // Scalar Set element (string/bool/f64/i64)
                                    let set_fn_name = if elem == "string" || elem == "str" {
                                        "mimi_set_to_display_string"
                                    } else if elem == "bool" {
                                        "mimi_set_to_display_bool"
                                    } else if elem == "f64" || elem == "f32" {
                                        "mimi_set_to_display_f64"
                                    } else {
                                        "mimi_set_to_display"
                                    };
                                    let set_fn = self.get_runtime_fn(set_fn_name)?;
                                    self.build_call(
                                        set_fn,
                                        &[BasicMetadataValueEnum::IntValue(as_i64)],
                                        "result_set_disp",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("set display void")?
                                    .into_pointer_value()
                                }
                            } else {
                                let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                                self.build_call(
                                    set_fn,
                                    &[BasicMetadataValueEnum::IntValue(as_i64)],
                                    "result_set_disp",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("set display void")?
                                .into_pointer_value()
                            }
                        } else if ok_root.starts_with("Map<") || arg_type.contains("Map<") {
                            let map_inner = arg_type
                                .strip_prefix("Result<")
                                .and_then(|s| {
                                    // Result<Map<…>, E> — take first type arg.
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
                                .unwrap_or(arg_type);
                            if let Some(val_ty) = map_inner
                                .strip_prefix("Map<string, ")
                                .and_then(|s| s.strip_suffix('>'))
                            {
                                if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                    let elem = if self.is_product_tuple_alias(val_ty) {
                                        self.resolve_alias_type_name(val_ty)
                                    } else {
                                        val_ty.to_string()
                                    };
                                    self.emit_map_product_to_json(as_i64, &elem, 1)?
                                } else if let Some(opt_inner) = val_ty
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
                                        self.emit_map_option_product_to_json(as_i64, &elem, 1)?
                                    } else {
                                        let fn_name = Self::map_json_fn_for_type(arg_type);
                                        let func = self.get_runtime_fn(fn_name)?;
                                        self.build_call(
                                            func,
                                            &[BasicMetadataValueEnum::IntValue(as_i64)],
                                            "res_ok_map",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("map to_json void")?
                                        .into_pointer_value()
                                    }
                                } else if let Some(list_elem) = val_ty
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
                                        self.emit_map_list_product_to_json(as_i64, &elem, 1)?
                                    } else {
                                        let fn_name = Self::map_json_fn_for_type(arg_type);
                                        let func = self.get_runtime_fn(fn_name)?;
                                        self.build_call(
                                            func,
                                            &[BasicMetadataValueEnum::IntValue(as_i64)],
                                            "res_ok_map",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("map to_json void")?
                                        .into_pointer_value()
                                    }
                                } else if let Some(set_elem) = val_ty
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
                                        self.emit_map_set_product_to_json(as_i64, &elem, 1)?
                                    } else {
                                        let fn_name = Self::map_json_fn_for_type(arg_type);
                                        let func = self.get_runtime_fn(fn_name)?;
                                        self.build_call(
                                            func,
                                            &[BasicMetadataValueEnum::IntValue(as_i64)],
                                            "res_ok_map",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("map to_json void")?
                                        .into_pointer_value()
                                    }
                                } else if val_ty.starts_with("Map<string, ") {
                                    if let Some(inner_val) = val_ty
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
                                            self.emit_map_map_product_to_json(as_i64, &elem, 1)?
                                        } else {
                                            let fn_name = Self::map_json_fn_for_type(arg_type);
                                            let func = self.get_runtime_fn(fn_name)?;
                                            self.build_call(
                                                func,
                                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                                "res_ok_map",
                                            )?
                                            .try_as_basic_value_opt()
                                            .ok_or("map to_json void")?
                                            .into_pointer_value()
                                        }
                                    } else {
                                        let fn_name = Self::map_json_fn_for_type(arg_type);
                                        let func = self.get_runtime_fn(fn_name)?;
                                        self.build_call(
                                            func,
                                            &[BasicMetadataValueEnum::IntValue(as_i64)],
                                            "res_ok_map",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("map to_json void")?
                                        .into_pointer_value()
                                    }
                                } else {
                                    let fn_name = Self::map_json_fn_for_type(arg_type);
                                    let func = self.get_runtime_fn(fn_name)?;
                                    self.build_call(
                                        func,
                                        &[BasicMetadataValueEnum::IntValue(as_i64)],
                                        "res_ok_map",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("map to_json void")?
                                    .into_pointer_value()
                                }
                            } else {
                                let fn_name = Self::map_json_fn_for_type(arg_type);
                                let func = self.get_runtime_fn(fn_name)?;
                                self.build_call(
                                    func,
                                    &[BasicMetadataValueEnum::IntValue(as_i64)],
                                    "res_ok_map",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("map to_json void")?
                                .into_pointer_value()
                            }
                        } else {
                            let set_fn = self.get_runtime_fn("mimi_set_to_display")?;
                            self.build_call(
                                set_fn,
                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                "result_set_disp",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("set display void")?
                            .into_pointer_value()
                        };
                        let wrap = self.emit_display_wrap("Ok(", disp, "res_ok_ms_wrap")?;
                        self.build_store(out_slot, wrap)?;
                        self.flush_display_since(arm_marker)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        return Ok(());
                    }
                    // Result<T,string> stores Err as ptrtoint of heap {ptr,len} string.
                    // When the declared Err type is string, always decode as string
                    // (non-zero handle). Otherwise only decode when value looks like
                    // a heap pointer (>= 1MB, 8-byte aligned); small integers stay numeric.
                    let err_ty_is_string = {
                        // Result<Ok, Err> — last top-level type arg is string.
                        arg_type
                            .strip_prefix("Result<")
                            .and_then(|s| {
                                let mut depth = 0i32;
                                let mut last_comma = None;
                                for (i, ch) in s.char_indices() {
                                    match ch {
                                        '<' | '(' => depth += 1,
                                        '>' | ')' => depth -= 1,
                                        ',' if depth == 0 => last_comma = Some(i),
                                        _ => {}
                                    }
                                }
                                last_comma.map(|c| s[c + 1..].trim().trim_end_matches('>').trim())
                            })
                            .map(|e| e == "string" || e == "str")
                            .unwrap_or(false)
                    };
                    let min_heap = i64_ty.const_int(1_048_576, false);
                    let is_heapish = if iv.get_type().get_bit_width() == 64 {
                        if err_ty_is_string && label == "err" {
                            // Typed string Err: any non-null handle is a string.
                            self.builder
                                .build_int_compare(
                                    IntPredicate::NE,
                                    as_i64,
                                    i64_ty.const_int(0, false),
                                    &format!("{}_str_nz", label),
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            let ge = self
                                .builder
                                .build_int_compare(
                                    IntPredicate::UGE,
                                    as_i64,
                                    min_heap,
                                    &format!("{}_ge_heap", label),
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let and7 = self
                                .builder
                                .build_and(
                                    as_i64,
                                    i64_ty.const_int(7, false),
                                    &format!("{}_and7", label),
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let aligned = self
                                .builder
                                .build_int_compare(
                                    IntPredicate::EQ,
                                    and7,
                                    i64_ty.const_int(0, false),
                                    &format!("{}_aligned", label),
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder
                                .build_and(ge, aligned, &format!("{}_heapish", label))
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        }
                    } else {
                        self.context.bool_type().const_int(0, false)
                    };

                    let parent_fn = self
                        .builder
                        .get_insert_block()
                        .and_then(|bb| bb.get_parent())
                        .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
                    let str_bb = self
                        .context
                        .append_basic_block(parent_fn, &format!("res_{}_str", label));
                    let int_bb = self
                        .context
                        .append_basic_block(parent_fn, &format!("res_{}_int", label));
                    let arm_merge = self
                        .context
                        .append_basic_block(parent_fn, &format!("res_{}_arm_merge", label));
                    self.builder
                        .build_conditional_branch(is_heapish, str_bb, int_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;

                    self.builder.position_at_end(str_bb);
                    {
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        if label == "err" && err_ty_is_string {
                            // Err(string): the legacy slot holds either a bare
                            // NUL-terminated data pointer (runtime strings,
                            // e.g. str_repeat payloads) or a {ptr,len} struct
                            // pointer (string literals). Probe field0: if the
                            // 8 bytes at the handle look like a mapped pointer,
                            // decode the struct; otherwise treat the handle as
                            // the data pointer. (0.34.36 probes: strlen of the
                            // data case SIGSEGVed both at HEAD and after the
                            // first naive fix; mincore distinguishes the two.)
                            let as_ptr = self
                                .builder
                                .build_int_to_ptr(as_i64, i8_ptr, "res_err_pp")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let field0 = self
                                .builder
                                .build_load(
                                    BasicTypeEnum::IntType(i64_ty),
                                    as_ptr,
                                    "res_err_field0",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                .into_int_value();
                            let field0_ptr = self
                                .builder
                                .build_int_to_ptr(field0, i8_ptr, "res_err_field0p")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let probe_fn = self
                                .module
                                .get_function("mimi_runtime_ptr_readable")
                                .unwrap_or_else(|| {
                                    self.module.add_function(
                                        "mimi_runtime_ptr_readable",
                                        i64_ty.fn_type(
                                            &[
                                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                                BasicMetadataTypeEnum::IntType(i64_ty),
                                            ],
                                            false,
                                        ),
                                        Some(inkwell::module::Linkage::External),
                                    )
                                });
                            let probe = self
                                .builder
                                .build_call(
                                    probe_fn,
                                    &[field0_ptr.into(), i64_ty.const_int(8, false).into()],
                                    "res_err_probe",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let probe_ok = self
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    probe
                                        .try_as_basic_value_opt()
                                        .ok_or_else(|| {
                                            CompileError::LlvmError(
                                                "ptr_readable returned void".into(),
                                            )
                                        })?
                                        .into_int_value(),
                                    i64_ty.const_zero(),
                                    "res_err_probe_ok",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let struct_bb =
                                self.context.append_basic_block(parent_fn, "res_err_struct");
                            let data_bb =
                                self.context.append_basic_block(parent_fn, "res_err_data");
                            let err_merge =
                                self.context.append_basic_block(parent_fn, "res_err_merge");
                            self.builder
                                .build_conditional_branch(probe_ok, struct_bb, data_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;

                            self.builder.position_at_end(struct_bb);
                            {
                                let str_sty = self.context.struct_type(
                                    &[
                                        BasicTypeEnum::PointerType(i8_ptr),
                                        BasicTypeEnum::IntType(i64_ty),
                                    ],
                                    false,
                                );
                                let loaded = self
                                    .builder
                                    .build_load(
                                        BasicTypeEnum::StructType(str_sty),
                                        as_ptr,
                                        "res_err_str_ld",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                    .into_struct_value();
                                let data_ptr = self
                                    .build_extract_value(loaded.into(), 0, "res_err_data")?
                                    .into_pointer_value();
                                let wrap = self.emit_display_wrap(
                                    wrap_prefix,
                                    data_ptr,
                                    "res_err_wrap_s",
                                )?;
                                self.build_store(out_slot, wrap)?;
                            }
                            self.builder
                                .build_unconditional_branch(err_merge)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;

                            self.builder.position_at_end(data_bb);
                            {
                                let wrap =
                                    self.emit_display_wrap(wrap_prefix, as_ptr, "res_err_wrap_d")?;
                                self.build_store(out_slot, wrap)?;
                            }
                            self.builder
                                .build_unconditional_branch(err_merge)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;

                            self.builder.position_at_end(err_merge);
                        } else {
                            // Heap {ptr,len} string struct fallback: decode
                            // field 0 as the data pointer.
                            let str_sty = self.context.struct_type(
                                &[
                                    BasicTypeEnum::PointerType(i8_ptr),
                                    BasicTypeEnum::IntType(i64_ty),
                                ],
                                false,
                            );
                            let as_ptr = self
                                .builder
                                .build_int_to_ptr(as_i64, i8_ptr, &format!("{}_as_ptr", label))
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let loaded = self
                                .builder
                                .build_load(
                                    BasicTypeEnum::StructType(str_sty),
                                    as_ptr,
                                    &format!("{}_str_ld", label),
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                .into_struct_value();
                            let data_ptr = self
                                .build_extract_value(loaded.into(), 0, &format!("{}_data", label))?
                                .into_pointer_value();
                            let wrap = self.emit_display_wrap(
                                wrap_prefix,
                                data_ptr,
                                &format!("res_{}_wrap_s", label),
                            )?;
                            self.build_store(out_slot, wrap)?;
                        }
                    }
                    self.builder
                        .build_unconditional_branch(arm_merge)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;

                    self.builder.position_at_end(int_bb);
                    {
                        let arm_marker = self.display_marker();
                        let pay_str =
                            self.emit_display_i64_str(as_i64, &format!("res_{}_pay", label))?;
                        let wrap = self.emit_display_wrap(
                            wrap_prefix,
                            pay_str,
                            &format!("res_{}_wrap_i", label),
                        )?;
                        self.flush_display_since(arm_marker)?;
                        self.build_store(out_slot, wrap)?;
                    }
                    self.builder
                        .build_unconditional_branch(arm_merge)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(arm_merge);
                }
                BasicTypeEnum::StructType(sty) => {
                    let fields_st = sty.get_field_types();
                    let sv = val.into_struct_value();
                    // Nested Result {i1, ok, err}
                    if fields_st.len() >= 3
                        && matches!(
                            fields_st[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        )
                    {
                        let nested = self.emit_result_to_string(sv, None)?;
                        let wrap = self.emit_display_wrap(
                            wrap_prefix,
                            nested,
                            &format!("res_{}_wrap_n", label),
                        )?;
                        self.build_store(out_slot, wrap)?;
                    } else if fields_st.len() == 2
                        && matches!(
                            fields_st[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        )
                    {
                        // Nested Option — pass full Option<…> type from Result Ok arm.
                        let opt_ty = Self::strip_first_type_arg(arg_type, "Result")
                            .unwrap_or_else(|| "Option".to_string());
                        let nested = self.emit_option_to_string(sv, None, &opt_ty)?;
                        let wrap = self.emit_display_wrap(
                            wrap_prefix,
                            nested,
                            &format!("res_{}_wrap_o", label),
                        )?;
                        self.build_store(out_slot, wrap)?;
                    } else if fields_st.len() == 2
                        && matches!(
                            fields_st[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 32
                        )
                        && matches!(
                            fields_st[1],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        )
                    {
                        // Nested custom enum {i32, i64}
                        let ok_inner =
                            Self::strip_first_type_arg(arg_type, "Result").unwrap_or_default();
                        let enum_ty =
                            if self.type_defs.get(&ok_inner).is_some_and(|td| {
                                matches!(td.kind, crate::ast::TypeDefKind::Enum(_))
                            }) {
                                Some(ok_inner.as_str())
                            } else {
                                self.type_defs.iter().find_map(|(n, td)| {
                                    if matches!(td.kind, crate::ast::TypeDefKind::Enum(_)) {
                                        Some(n.as_str())
                                    } else {
                                        None
                                    }
                                })
                            };
                        if let Some(et) = enum_ty {
                            let nested = self.emit_enum_display(et, sv)?;
                            let wrap = self.emit_display_wrap(
                                wrap_prefix,
                                nested,
                                &format!("res_{}_wrap_e", label),
                            )?;
                            self.build_store(out_slot, wrap)?;
                        }
                    } else if fields_st.len() >= 1
                        && matches!(fields_st[0], BasicTypeEnum::PointerType(_))
                    {
                        // string {ptr,len}
                        let ptr = self
                            .build_extract_value(sv.into(), 0, &format!("{}_str", label))?
                            .into_pointer_value();
                        let wrap = self.emit_display_wrap(
                            wrap_prefix,
                            ptr,
                            &format!("res_{}_wrap_s2", label),
                        )?;
                        self.build_store(out_slot, wrap)?;
                    }
                }
                // 0.34.24: scalar float payload (Result<f64, E> Ok arm /
                // Option<f64> etc.). Pre-fix the float fell through to the
                // "Ok(?)" catch-all — display divergence vs the VM
                // (audit follow-up found via the Result<f64,string> crash).
                BasicTypeEnum::FloatType(_) => {
                    let fv = val.into_float_value();
                    // mimi_to_string_f64 (shortest round-trip) replaces %g —
                    // matches the VM and the scalar print path.
                    let arm_marker = self.display_marker();
                    let pay_str = self.emit_display_f64_str(fv, &format!("res_{}_pay_f", label))?;
                    let wrap = self.emit_display_wrap(
                        wrap_prefix,
                        pay_str,
                        &format!("res_{}_wrap_f", label),
                    )?;
                    self.flush_display_since(arm_marker)?;
                    self.build_store(out_slot, wrap)?;
                }
                _ => {
                    let unk = self.emit_display_lit_copy(
                        if label == "ok" { "Ok(?)" } else { "Err(?)" },
                        &format!("res_{}_unk", label),
                    )?;
                    self.build_store(out_slot, unk)?;
                }
            }
            self.flush_display_since(arm_marker)?;
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            Ok(())
        };

        emit_arm("ok", ok_bb, ok_val, fields[1])?;
        emit_arm("err", err_bb, err_val, fields[2])?;
        self.builder.position_at_end(merge_bb);
        let disp = self
            .build_load(
                BasicTypeEnum::PointerType(i8_ptr_ty),
                out_slot,
                "res_disp_ld",
            )?
            .into_pointer_value();
        // Defined on every runtime path; the consuming print's
        // flush_display_frees releases it exactly once.
        self.register_display_alloc(disp);
        Ok(disp)
    }

    /// Format Option {i1, i64} as `Some(...)` / `None()` matching interp Display.
    /// When `inner_record` is Some, payload is ptrtoint of that record type.
    fn emit_option_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        inner_record: Option<&str>,
        arg_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let disc = self
            .build_extract_value(sv.into(), 0, "opt_disc")?
            .into_int_value();
        // 0.39.136: `Option<()>` — the unit payload occupies an i64 sentinel
        // slot (ABI parity with the resolved emitter), so the Int classifier
        // below would render `Some(0)`. The interpreter prints `Some(())`;
        // mirror it by selecting between the literal strings on the tag.
        {
            let inner_root = arg_type
                .strip_prefix("Option<")
                .and_then(|s| s.strip_suffix('>'))
                .map(|s| s.trim())
                .unwrap_or("");
            if inner_root == "()" || inner_root == "unit" {
                let some_str = self
                    .builder
                    .build_global_string_ptr("Some(())", "opt_unit_some")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let none_str = self
                    .builder
                    .build_global_string_ptr("None()", "opt_unit_none")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let zero = disc.get_type().const_int(0, false);
                let is_some = self
                    .builder
                    .build_int_compare(IntPredicate::NE, disc, zero, "opt_unit_is_some")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let sel = self
                    .builder
                    .build_select(
                        is_some,
                        some_str.as_pointer_value(),
                        none_str.as_pointer_value(),
                        "opt_unit_sel",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                return Ok(sel.into_pointer_value());
            }
        }
        let payload_bv = self.build_extract_value(sv.into(), 1, "opt_pay")?;
        // Classify payload for Some(...) formatting.
        enum OptPay<'a> {
            Int(inkwell::values::IntValue<'a>),
            Float(inkwell::values::FloatValue<'a>),
            StrPtr(inkwell::values::PointerValue<'a>),
            RecPtr(inkwell::values::PointerValue<'a>),
            /// Nested Option payload is ptrtoint of heap Option; load only in Some arm.
            NestedOpt(inkwell::values::IntValue<'a>),
            /// Nested Result payload is ptrtoint of heap Result; load only in Some arm.
            NestedRes(inkwell::values::IntValue<'a>),
            /// List payload is ptrtoint (or pointer) of list struct; load only in Some arm
            /// so None's null/garbage handle does not SIGSEGV.
            NestedList(inkwell::values::IntValue<'a>),
        }
        let pay_kind = match payload_bv {
            BasicValueEnum::IntValue(iv) => {
                let bw = iv.get_type().get_bit_width();
                if bw == 1 && inner_record.is_none() {
                    // Bool payload: print true/false via string path.
                    let true_g = self
                        .builder
                        .build_global_string_ptr("true", "opt_bool_t")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let false_g = self
                        .builder
                        .build_global_string_ptr("false", "opt_bool_f")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let zero = iv.get_type().const_int(0, false);
                    let is_t = self
                        .builder
                        .build_int_compare(IntPredicate::NE, iv, zero, "opt_bool")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let sel = self
                        .builder
                        .build_select(
                            is_t,
                            true_g.as_pointer_value(),
                            false_g.as_pointer_value(),
                            "opt_bool_s",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    OptPay::StrPtr(sel.into_pointer_value())
                } else {
                    let as_i64 = if bw < 64 {
                        self.builder
                            .build_int_s_extend(iv, i64_ty, "opt_pay_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    };
                    if inner_record.is_some() {
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let p = self
                            .builder
                            .build_int_to_ptr(as_i64, i8_ptr, "opt_rec_from_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        OptPay::RecPtr(p)
                    } else if arg_type == "Option<bool>"
                        || arg_type.ends_with("<bool>")
                        || arg_type.contains("bool>")
                    {
                        // Bool stored as i64 0/1: print true/false.
                        let true_g = self
                            .builder
                            .build_global_string_ptr("true", "opt_bool_t2")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let false_g = self
                            .builder
                            .build_global_string_ptr("false", "opt_bool_f2")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let zero = i64_ty.const_int(0, false);
                        let is_t = self
                            .builder
                            .build_int_compare(IntPredicate::NE, as_i64, zero, "opt_bool2")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let sel = self
                            .builder
                            .build_select(
                                is_t,
                                true_g.as_pointer_value(),
                                false_g.as_pointer_value(),
                                "opt_bool_s2",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        OptPay::StrPtr(sel.into_pointer_value())
                    } else if {
                        // Only Option of Map (not Option of List/Result of Map).
                        let inner = arg_type
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                            .unwrap_or(arg_type);
                        inner.starts_with("Map") || arg_type == "Option<Map>"
                    } {
                        // Option of Map of product: decode heap product values.
                        let map_inner = arg_type
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                            .unwrap_or(arg_type);
                        let raw = if let Some(val_ty) = map_inner
                            .strip_prefix("Map<string, ")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if val_ty.starts_with('(') || self.is_product_tuple_alias(val_ty) {
                                let elem = if self.is_product_tuple_alias(val_ty) {
                                    self.resolve_alias_type_name(val_ty)
                                } else {
                                    val_ty.to_string()
                                };
                                self.emit_map_product_to_json(as_i64, &elem, 1)?
                            } else if val_ty.starts_with("Map<string, ") {
                                if let Some(inner_val) = val_ty
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
                                        self.emit_map_map_product_to_json(as_i64, &elem, 1)?
                                    } else {
                                        let fn_name = Self::map_json_fn_for_type(arg_type);
                                        let func = self.get_runtime_fn(fn_name)?;
                                        self.build_call(
                                            func,
                                            &[BasicMetadataValueEnum::IntValue(as_i64)],
                                            "opt_map_json",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("map to_json void")?
                                        .into_pointer_value()
                                    }
                                } else {
                                    let fn_name = Self::map_json_fn_for_type(arg_type);
                                    let func = self.get_runtime_fn(fn_name)?;
                                    self.build_call(
                                        func,
                                        &[BasicMetadataValueEnum::IntValue(as_i64)],
                                        "opt_map_json",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("map to_json void")?
                                    .into_pointer_value()
                                }
                            } else if let Some(list_elem) = val_ty
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
                                    self.emit_map_list_product_to_json(as_i64, &elem, 1)?
                                } else {
                                    let fn_name = Self::map_json_fn_for_type(arg_type);
                                    let func = self.get_runtime_fn(fn_name)?;
                                    self.build_call(
                                        func,
                                        &[BasicMetadataValueEnum::IntValue(as_i64)],
                                        "opt_map_json",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("map to_json void")?
                                    .into_pointer_value()
                                }
                            } else if let Some(set_elem) = val_ty
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
                                    self.emit_map_set_product_to_json(as_i64, &elem, 1)?
                                } else {
                                    let fn_name = Self::map_json_fn_for_type(arg_type);
                                    let func = self.get_runtime_fn(fn_name)?;
                                    self.build_call(
                                        func,
                                        &[BasicMetadataValueEnum::IntValue(as_i64)],
                                        "opt_map_json",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("map to_json void")?
                                    .into_pointer_value()
                                }
                            } else {
                                let fn_name = Self::map_json_fn_for_type(arg_type);
                                let func = self.get_runtime_fn(fn_name)?;
                                self.build_call(
                                    func,
                                    &[BasicMetadataValueEnum::IntValue(as_i64)],
                                    "opt_map_json",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("map to_json void")?
                                .into_pointer_value()
                            }
                        } else {
                            let fn_name = Self::map_json_fn_for_type(arg_type);
                            let func = self.get_runtime_fn(fn_name)?;
                            self.build_call(
                                func,
                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                "opt_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("map to_json void")?
                            .into_pointer_value()
                        };
                        OptPay::StrPtr(raw)
                    } else if arg_type.contains("Set<") || arg_type == "Option<Set>" {
                        let set_inner = arg_type
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                            .unwrap_or(arg_type);
                        let raw = if let Some(elem) = set_inner
                            .strip_prefix("Set<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if elem.starts_with('(') || self.is_product_tuple_alias(elem) {
                                let resolved = if self.is_product_tuple_alias(elem) {
                                    self.resolve_alias_type_name(elem)
                                } else {
                                    elem.to_string()
                                };
                                // Display style for Option of Set product.
                                self.emit_set_product_to_json(as_i64, &resolved, 1)?
                            } else if elem.starts_with("Map<string, ") {
                                if let Some(val_ty) = elem
                                    .strip_prefix("Map<string, ")
                                    .and_then(|s| s.strip_suffix('>'))
                                {
                                    if val_ty.starts_with('(')
                                        || self.is_product_tuple_alias(val_ty)
                                    {
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
                                        let func = self
                                            .get_runtime_fn("mimi_set_to_json_map_product_i64")?;
                                        let i64_ty = self.context.i64_type();
                                        self.build_call(
                                            func,
                                            &[
                                                BasicMetadataValueEnum::IntValue(as_i64),
                                                BasicMetadataValueEnum::IntValue(
                                                    i64_ty.const_int(arity as u64, false),
                                                ),
                                                BasicMetadataValueEnum::IntValue(
                                                    i64_ty.const_int(1, false),
                                                ),
                                            ],
                                            "opt_set_map_disp",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("opt set map display void")?
                                        .into_pointer_value()
                                    } else {
                                        let fn_name = Self::set_display_fn_for_type(arg_type);
                                        let func = self.get_runtime_fn(fn_name)?;
                                        self.build_call(
                                            func,
                                            &[BasicMetadataValueEnum::IntValue(as_i64)],
                                            "opt_set_disp",
                                        )?
                                        .try_as_basic_value_opt()
                                        .ok_or("set display void")?
                                        .into_pointer_value()
                                    }
                                } else {
                                    let fn_name = Self::set_display_fn_for_type(arg_type);
                                    let func = self.get_runtime_fn(fn_name)?;
                                    self.build_call(
                                        func,
                                        &[BasicMetadataValueEnum::IntValue(as_i64)],
                                        "opt_set_disp",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("set display void")?
                                    .into_pointer_value()
                                }
                            } else {
                                let fn_name = Self::set_display_fn_for_type(arg_type);
                                let func = self.get_runtime_fn(fn_name)?;
                                self.build_call(
                                    func,
                                    &[BasicMetadataValueEnum::IntValue(as_i64)],
                                    "opt_set_disp",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("set display void")?
                                .into_pointer_value()
                            }
                        } else {
                            let fn_name = Self::set_display_fn_for_type(arg_type);
                            let func = self.get_runtime_fn(fn_name)?;
                            self.build_call(
                                func,
                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                "opt_set_disp",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("set display void")?
                            .into_pointer_value()
                        };
                        OptPay::StrPtr(raw)
                    } else if arg_type.contains("List<") || arg_type.starts_with("Option<List") {
                        // Defer list load until Some arm (None payload may be null).
                        OptPay::NestedList(as_i64)
                    } else if arg_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .is_some_and(|inner| inner.starts_with("Option"))
                    {
                        // Defer load of nested Option until Some arm (None has null payload).
                        OptPay::NestedOpt(as_i64)
                    } else if arg_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .is_some_and(|inner| inner.starts_with("Result"))
                    {
                        OptPay::NestedRes(as_i64)
                    } else {
                        OptPay::Int(as_i64)
                    }
                }
            }
            BasicValueEnum::FloatValue(fv) => OptPay::Float(fv),
            BasicValueEnum::PointerValue(pv) => {
                if inner_record.is_some() {
                    OptPay::RecPtr(pv)
                } else if arg_type.starts_with("Option<List") || arg_type.contains("List<") {
                    // Defer list load until Some arm.
                    let as_i64 = self.build_ptr_to_int(pv, i64_ty, "opt_list_ptr_i64")?;
                    OptPay::NestedList(as_i64)
                } else {
                    OptPay::StrPtr(pv)
                }
            }
            BasicValueEnum::StructValue(psv) => {
                let pfields = psv.get_type().get_field_types();
                // Nested Result {i1, ok, err} inside Option.
                if pfields.len() >= 3
                    && matches!(
                        pfields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                {
                    let res_ty = Self::strip_first_type_arg(arg_type, "Option")
                        .unwrap_or_else(|| "Result".to_string());
                    let ok_rec = res_ty
                        .strip_prefix("Result<")
                        .and_then(|s| s.split(',').next())
                        .map(|s| s.trim())
                        .filter(|inner| {
                            !inner.is_empty()
                                && self.type_defs.get(*inner).is_some_and(|td| {
                                    matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                })
                        });
                    let nested = self.emit_result_to_string_typed(psv, ok_rec, &res_ty)?;
                    OptPay::StrPtr(nested)
                } else if pfields.len() == 2
                    && matches!(
                        pfields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                {
                    // Nested Option {i1, ...}: recursive Display via emit_option_to_string.
                    // Strip one Option layer: Option<Option<List<i32>>> → Option<List<i32>>
                    let inner_ty = Self::strip_first_type_arg(arg_type, "Option")
                        .unwrap_or_else(|| "Option".to_string());
                    let nested = self.emit_option_to_string(psv, None, &inner_ty)?;
                    OptPay::StrPtr(nested)
                } else if pfields.len() == 2
                    && matches!(
                        pfields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 32
                    )
                    && matches!(
                        pfields[1],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    )
                {
                    // Nested custom enum {i32 tag, i64 payload}.
                    let enum_ty = arg_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .filter(|n| {
                            self.type_defs.get(*n).is_some_and(|td| {
                                matches!(td.kind, crate::ast::TypeDefKind::Enum(_))
                            })
                        })
                        .or_else(|| {
                            // Try find any enum type that matches layout — use first matching.
                            self.type_defs.iter().find_map(|(n, td)| {
                                if matches!(td.kind, crate::ast::TypeDefKind::Enum(_)) {
                                    Some(n.as_str())
                                } else {
                                    None
                                }
                            })
                        });
                    if let Some(et) = enum_ty {
                        let nested = self.emit_enum_display(et, psv)?;
                        OptPay::StrPtr(nested)
                    } else {
                        OptPay::Int(i64_ty.const_int(0, false))
                    }
                } else if pfields.len() >= 1 && matches!(pfields[0], BasicTypeEnum::PointerType(_))
                {
                    // string {ptr,len}
                    let dp = self
                        .build_extract_value(psv.into(), 0, "opt_str_ptr")?
                        .into_pointer_value();
                    OptPay::StrPtr(dp)
                } else if let Some(rec_name) = arg_type
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                    .filter(|n| {
                        self.type_defs
                            .get(*n)
                            .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
                    })
                {
                    // Named record by-value in Option payload.
                    let rec_ty = psv.get_type();
                    let tmp =
                        self.build_alloca(BasicTypeEnum::StructType(rec_ty), "opt_rec_disp_tmp")?;
                    self.build_store(tmp, psv)?;
                    let rec_str = self.emit_record_display(rec_name, tmp)?;
                    OptPay::StrPtr(rec_str)
                } else if pfields.len() == 2
                    && matches!(
                        pfields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    )
                    && matches!(pfields[1], BasicTypeEnum::PointerType(_))
                {
                    // List by-value in Option payload: {i64,ptr}.
                    let list_str = if arg_type.contains("List<string>") {
                        self.emit_list_string_to_string(psv)?
                    } else if arg_type.contains("Map<") {
                        self.emit_list_map_to_string(psv, "List")?
                    } else if let Some(inner) = arg_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .and_then(|s| s.strip_prefix("List<"))
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if inner.starts_with('(') || self.is_product_tuple_alias(inner) {
                            let elem = if self.is_product_tuple_alias(inner) {
                                self.resolve_alias_type_name(inner)
                            } else {
                                inner.to_string()
                            };
                            self.emit_list_product_tuple_to_string(psv, &elem)?
                        } else {
                            self.emit_list_i32_to_string(psv)?
                        }
                    } else {
                        self.emit_list_i32_to_string(psv)?
                    };
                    OptPay::StrPtr(list_str)
                } else if pfields.len() >= 2
                    && !matches!(
                        pfields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                {
                    // Product tuple / multi-field struct by-value (not nested
                    // Option/Result and not List {i64,ptr}).
                    let tup_str = self.emit_product_tuple_to_string(psv, Some(arg_type))?;
                    OptPay::StrPtr(tup_str)
                } else {
                    OptPay::Int(i64_ty.const_int(0, false))
                }
            }
            other => {
                return Err(CompileError::LlvmError(format!(
                    "option payload unexpected kind {:?}",
                    other
                )))
            }
        };
        // §8-#96/D-4 residue (0.34.36): exact-size assembly per arm — the
        // fixed 512-byte snprintf("Some(%s)") buffer silently truncated long
        // payloads. See emit_result_to_string_typed for the out_slot
        // discipline (unregistered arm wraps, merge registers).
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let out_slot =
            self.build_alloca(BasicTypeEnum::PointerType(i8_ptr_ty), "opt_print_slot")?;
        let opt_disp_marker = self.display_marker();
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent fn".into()))?;
        let some_bb = self.context.append_basic_block(parent, "opt_print_some");
        let none_bb = self.context.append_basic_block(parent, "opt_print_none");
        let merge_bb = self.context.append_basic_block(parent, "opt_print_merge");
        let zero = disc.get_type().const_int(0, false);
        let is_some = self
            .builder
            .build_int_compare(IntPredicate::NE, disc, zero, "opt_is_some")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(is_some, some_bb, none_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(some_bb);
        match pay_kind {
            OptPay::RecPtr(rec_ptr) => {
                let rec_name = inner_record.unwrap_or("Record");
                let rec_str = self.emit_record_display(rec_name, rec_ptr)?;
                let wrap = self.emit_display_wrap("Some(", rec_str, "opt_some_wrap_s")?;
                self.build_store(out_slot, wrap)?;
            }
            OptPay::StrPtr(sp) => {
                // C-string sources (bool-select globals, runtime display
                // helpers, nested Display results) — NUL-terminated. Heap
                // {ptr,len} string-struct payloads arrive via StructValue
                // classification instead; a heapish pointer heuristic here
                // misread malloc'd Display results as string structs
                // (0.34.36 OOM probe), so it was removed.
                let wrap = self.emit_display_wrap("Some(", sp, "opt_some_wrap_str")?;
                self.build_store(out_slot, wrap)?;
            }
            OptPay::Int(payload_i64) => {
                let pay_str = self.emit_display_i64_str(payload_i64, "opt_some_pay")?;
                let wrap = self.emit_display_wrap("Some(", pay_str, "opt_some_wrap_i")?;
                self.build_store(out_slot, wrap)?;
            }
            OptPay::NestedOpt(as_i64) => {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let nested_ptr = self
                    .builder
                    .build_int_to_ptr(as_i64, i8_ptr, "opt_nested_from_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let opt_sty = self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(self.context.bool_type()),
                        BasicTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                let loaded = self
                    .builder
                    .build_load(
                        BasicTypeEnum::StructType(opt_sty),
                        nested_ptr,
                        "opt_nested_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                let inner_ty = Self::strip_first_type_arg(arg_type, "Option")
                    .unwrap_or_else(|| "Option".to_string());
                let nested = self.emit_option_to_string(loaded, None, &inner_ty)?;
                let wrap = self.emit_display_wrap("Some(", nested, "opt_nested_wrap")?;
                self.build_store(out_slot, wrap)?;
            }
            OptPay::NestedRes(as_i64) => {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let nested_ptr = self
                    .builder
                    .build_int_to_ptr(as_i64, i8_ptr, "opt_res_from_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let res_sty = self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(self.context.bool_type()),
                        BasicTypeEnum::IntType(i64_ty),
                        BasicTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                let loaded = self
                    .builder
                    .build_load(BasicTypeEnum::StructType(res_sty), nested_ptr, "opt_res_ld")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                let res_ty = Self::strip_first_type_arg(arg_type, "Option")
                    .unwrap_or_else(|| "Result".to_string());
                let nested = self.emit_result_to_string_typed(loaded, None, &res_ty)?;
                let wrap = self.emit_display_wrap("Some(", nested, "opt_res_wrap")?;
                self.build_store(out_slot, wrap)?;
            }
            OptPay::NestedList(as_i64) => {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let list_ptr = self
                    .builder
                    .build_int_to_ptr(as_i64, i8_ptr, "opt_list_from_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let list_ty = self.list_struct_type();
                let loaded = self
                    .builder
                    .build_load(BasicTypeEnum::StructType(list_ty), list_ptr, "opt_list_ld")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                let list_str = if arg_type.contains("List<string>") {
                    self.emit_list_string_to_string(loaded)?
                } else if let Some(inner) = arg_type
                    .strip_prefix("Option<")
                    .and_then(|s| s.strip_suffix('>'))
                    .and_then(|s| s.strip_prefix("List<"))
                    .and_then(|s| s.strip_suffix('>'))
                {
                    if inner.starts_with("List<") {
                        let mid = Self::strip_first_type_arg(&format!("List<{}>", inner), "List")
                            .unwrap_or_else(|| inner.to_string());
                        let mid_elem =
                            Self::strip_first_type_arg(&mid, "List").unwrap_or_else(|| mid.clone());
                        if mid_elem.starts_with('(') || self.is_product_tuple_alias(&mid_elem) {
                            let elem = if self.is_product_tuple_alias(&mid_elem) {
                                self.resolve_alias_type_name(&mid_elem)
                            } else {
                                mid_elem
                            };
                            self.emit_list_list_product_tuple_to_string(loaded, &elem)?
                        } else {
                            self.emit_list_i32_to_string(loaded)?
                        }
                    } else if inner.starts_with('(') || self.is_product_tuple_alias(inner) {
                        let elem = if self.is_product_tuple_alias(inner) {
                            self.resolve_alias_type_name(inner)
                        } else {
                            inner.to_string()
                        };
                        self.emit_list_product_tuple_to_string(loaded, &elem)?
                    } else if inner.starts_with("Option") {
                        self.emit_list_option_to_string(loaded, inner)?
                    } else if inner.starts_with("Result") {
                        self.emit_list_result_to_string(loaded, inner)?
                    } else if inner.starts_with("Map") {
                        self.emit_list_map_to_string(loaded, inner)?
                    } else if inner.starts_with("Set") {
                        self.emit_list_set_to_string(loaded, inner)?
                    } else if arg_type.contains("Map") {
                        self.emit_list_map_to_string(loaded, "List")?
                    } else {
                        self.emit_list_i32_to_string(loaded)?
                    }
                } else {
                    self.emit_list_i32_to_string(loaded)?
                };
                let wrap = self.emit_display_wrap("Some(", list_str, "opt_list_wrap")?;
                self.build_store(out_slot, wrap)?;
            }
            OptPay::Float(fv) => {
                let pay_str = self.emit_display_f64_str(fv, "opt_some_pay_f")?;
                let wrap = self.emit_display_wrap("Some(", pay_str, "opt_some_wrap_f")?;
                self.build_store(out_slot, wrap)?;
            }
        }
        self.flush_display_since(opt_disp_marker)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(none_bb);
        let none_lit = self.emit_display_lit_copy("None()", "opt_none_lit")?;
        self.build_store(out_slot, none_lit)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(merge_bb);
        let disp = self
            .build_load(
                BasicTypeEnum::PointerType(i8_ptr_ty),
                out_slot,
                "opt_disp_ld",
            )?
            .into_pointer_value();
        // Defined on every runtime path; the consuming print's
        // flush_display_frees releases it exactly once.
        self.register_display_alloc(disp);
        Ok(disp)
    }

    /// List of Map with nested product values (flat/List/Set/Map product).
    /// Uses map_nested_product_mode to pick runtime map product encoding.
    pub(in crate::codegen) fn emit_list_map_nested_product_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        map_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_map_nest_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_map_nest_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let mode = self.map_nested_product_mode(map_type);
        // D-4: exact-size two-pass assembly instead of the fixed 8192-byte
        // strcat loop. Elements are plain Map handles (no branching), so each
        // piece is defined unconditionally and the sized helper's flush frees
        // it exactly once per iteration (the old loop leaked every element's
        // runtime-allocated map JSON string).
        self.emit_sized_list_of_pieces(
            len,
            ",",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_slot = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_map_nest_slot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let handle = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_map_nest_handle")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                // Direct nested product helpers (mode 10+); never route 50+/60+
                // through mimi_result_map_to_json (only understands 0-3 and 10-40).
                let map_str = if mode >= 10 {
                    let (fn_name, arity) = if mode >= 60 {
                        ("mimi_map_to_json_result_product_i64", mode - 60)
                    } else if mode >= 50 {
                        ("mimi_map_to_json_option_product_i64", mode - 50)
                    } else if mode >= 40 {
                        ("mimi_map_to_json_map_product_i64", mode - 40)
                    } else if mode >= 30 {
                        ("mimi_map_to_json_set_product_i64", mode - 30)
                    } else if mode >= 20 {
                        ("mimi_map_to_json_list_product_i64", mode - 20)
                    } else {
                        ("mimi_map_to_json_product_i64", mode - 10)
                    };
                    let func = self.get_runtime_fn(fn_name)?;
                    self.build_call(
                        func,
                        &[
                            BasicMetadataValueEnum::IntValue(handle),
                            BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                            BasicMetadataValueEnum::IntValue(i64_ty.const_int(0, false)),
                        ],
                        "list_map_nest_direct",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("map nest direct void")?
                    .into_pointer_value()
                } else {
                    let map_fn = self.get_runtime_fn("mimi_map_to_json_i64")?;
                    self.build_call(
                        map_fn,
                        &[BasicMetadataValueEnum::IntValue(handle)],
                        "list_map_nest_scalar",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("map nest scalar void")?
                    .into_pointer_value()
                };
                // Runtime helpers return malloc'd C strings; register so the
                // sized helper's per-iteration flush frees them.
                self.register_display_alloc(map_str);
                Ok(map_str)
            },
            "list_map_nest_json",
        )
    }

    /// List of Map of product-tuple: JSON array of product map objects.
    pub(in crate::codegen) fn emit_list_map_product_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        product_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        // Reuse Display loop but with JSON style (display_style=0) and no spaces.
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_map_prod_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_map_prod_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        // D-4: exact-size two-pass assembly instead of the fixed 8192-byte
        // strcat loop. Elements are plain Map handles (no branching), so each
        // piece is defined unconditionally and the sized helper's flush frees
        // it exactly once per iteration (the old loop leaked every element's
        // runtime-allocated map JSON string).
        self.emit_sized_list_of_pieces(
            len,
            ",",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_slot = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_map_prod_slot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let handle = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_map_prod_handle")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                // emit_map_product_to_json returns a runtime malloc'd C string
                // (unregistered); register so the sized helper frees it.
                let map_str = self.emit_map_product_to_json(handle, product_type, 0)?;
                self.register_display_alloc(map_str);
                Ok(map_str)
            },
            "list_map_prod_json",
        )
    }

    pub(in crate::codegen) fn emit_set_result_option_product_to_json(
        &self,
        handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_set_to_json_result_option_product_i64")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "set_result_option_product_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("set result option product to_json void")?
            .into_pointer_value())
    }

    /// Set of Result of product-tuple values.
    pub(in crate::codegen) fn emit_set_result_product_to_json(
        &self,
        handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_set_to_json_result_product_i64")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "set_result_product_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("set result product to_json void")?
            .into_pointer_value())
    }

    /// Set of Option of Result of product-tuple values.
    pub(in crate::codegen) fn emit_set_option_result_product_to_json(
        &self,
        handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_set_to_json_option_result_product_i64")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "set_option_result_product_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("set option result product to_json void")?
            .into_pointer_value())
    }

    /// Set of Option of product-tuple values.
    pub(in crate::codegen) fn emit_set_option_product_to_json(
        &self,
        handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_set_to_json_option_product_i64")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "set_option_product_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("set option product to_json void")?
            .into_pointer_value())
    }

    /// Set of product-tuple values.
    pub(in crate::codegen) fn emit_set_product_to_json(
        &self,
        set_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let inner = product_type
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(product_type);
        let mut arity: i64 = 0;
        let mut depth = 0i32;
        let mut any = false;
        for ch in inner.chars() {
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
        if arity <= 0 {
            arity = 2;
        }
        let func = self.get_runtime_fn("mimi_set_to_json_product_i64")?;
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(set_handle),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "set_product_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("set product to_json void")?
            .into_pointer_value())
    }

    /// List of Result of Set of product via runtime.
    pub(in crate::codegen) fn emit_list_result_set_product_runtime(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_list_result_set_product_to_json")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(list_alloca),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "list_res_set_prod_rt",
            )?
            .try_as_basic_value_opt()
            .ok_or("list result set product runtime void")?
            .into_pointer_value())
    }

    /// List of Result of Map of product via runtime.
    pub(in crate::codegen) fn emit_list_result_map_product_runtime(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_list_result_map_product_to_json")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(list_alloca),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "list_res_map_prod_rt",
            )?
            .try_as_basic_value_opt()
            .ok_or("list result map product runtime void")?
            .into_pointer_value())
    }

    /// List of Result of product via runtime uniform heap pack.
    pub(in crate::codegen) fn emit_list_result_product_runtime(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = {
            let body = product_type
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(product_type);
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
        let func = self.get_runtime_fn("mimi_list_result_product_to_json")?;
        let i64_ty = self.context.i64_type();
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(list_alloca),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "list_res_prod_rt",
            )?
            .try_as_basic_value_opt()
            .ok_or("list result product runtime void")?
            .into_pointer_value())
    }

    /// List of Result of Set of product — JSON via per-element result_set product.
    #[allow(dead_code)]
    pub(in crate::codegen) fn emit_list_result_set_product_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        arity: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_res_set_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_res_set_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        // D-4: exact-size two-pass assembly instead of the fixed 8192-byte
        // strcat loop. Elements are Result heap packs (no branching in codegen
        // — the runtime helper resolves Ok/Err), so each piece is defined
        // unconditionally and the sized helper's flush frees it exactly once
        // per iteration (the old loop leaked every element's piece).
        self.emit_sized_list_of_pieces(
            len,
            ",",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_slot = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_res_set_slot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let handle_i64 = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_res_set_h")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                // Result heap pack {i1, i64 ok, i64 err}.
                let res_ptr = self
                    .builder
                    .build_int_to_ptr(handle_i64, i8_ptr, "list_res_set_res_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let disc_i8 = self
                    .builder
                    .build_load(self.context.i8_type(), res_ptr, "list_res_set_disc")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let disc_i64 = self
                    .builder
                    .build_int_z_extend(disc_i8, i64_ty, "list_res_set_disc_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let ok_slot = unsafe {
                    self.builder
                        .build_gep(
                            i64_ty,
                            res_ptr,
                            &[i64_ty.const_int(1, false)],
                            "list_res_set_ok",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let ok_h = self
                    .builder
                    .build_load(i64_ty, ok_slot, "list_res_set_ok_h")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let err_slot = unsafe {
                    self.builder
                        .build_gep(
                            i64_ty,
                            res_ptr,
                            &[i64_ty.const_int(2, false)],
                            "list_res_set_err",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let err_h = self
                    .builder
                    .build_load(i64_ty, err_slot, "list_res_set_err_h")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let mode = i64_ty.const_int((10 + arity) as u64, false);
                let res_fn = self.get_runtime_fn("mimi_result_set_to_json")?;
                let piece = self
                    .build_call(
                        res_fn,
                        &[
                            BasicMetadataValueEnum::IntValue(disc_i64),
                            BasicMetadataValueEnum::IntValue(ok_h),
                            BasicMetadataValueEnum::IntValue(err_h),
                            BasicMetadataValueEnum::IntValue(mode),
                        ],
                        "list_res_set_piece",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("result set to_json void")?
                    .into_pointer_value();
                // Runtime helper returns a malloc'd C string; register so the
                // sized helper's per-iteration flush frees it.
                self.register_display_alloc(piece);
                Ok(piece)
            },
            "list_res_set_json",
        )
    }

    /// List of Option of Set of product — JSON array of option set product.
    pub(in crate::codegen) fn emit_list_option_set_product_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        arity: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        // Reuse list option map-style loop: each element is Option {i1, i64 set handle}.
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_opt_set_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_opt_set_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        // D-4: exact-size two-pass assembly instead of the fixed 8192-byte
        // strcat loop. Elements are Option heap packs (no branching in codegen
        // — the runtime helper resolves Some/None), so each piece is defined
        // unconditionally and the sized helper's flush frees it exactly once
        // per iteration (the old loop leaked every element's piece).
        self.emit_sized_list_of_pieces(
            len,
            ",",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_slot = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_opt_set_slot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let handle_i64 = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_opt_set_h")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                // Option heap pack {i1, i64}: load disc and payload via inttoptr.
                let opt_ptr = self
                    .builder
                    .build_int_to_ptr(handle_i64, i8_ptr, "list_opt_set_opt_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let disc_i8 = self
                    .builder
                    .build_load(self.context.i8_type(), opt_ptr, "list_opt_set_disc")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let disc_i64 = self
                    .builder
                    .build_int_z_extend(disc_i8, i64_ty, "list_opt_set_disc_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let pay_slot = unsafe {
                    self.builder
                        .build_gep(
                            i64_ty,
                            opt_ptr,
                            &[i64_ty.const_int(1, false)],
                            "list_opt_set_pay",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let set_h = self
                    .builder
                    .build_load(i64_ty, pay_slot, "list_opt_set_pay_h")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                // mode = 10 + arity for product set.
                let mode = i64_ty.const_int((10 + arity) as u64, false);
                let opt_fn = self.get_runtime_fn("mimi_option_set_to_json")?;
                let piece = self
                    .build_call(
                        opt_fn,
                        &[
                            BasicMetadataValueEnum::IntValue(disc_i64),
                            BasicMetadataValueEnum::IntValue(set_h),
                            BasicMetadataValueEnum::IntValue(mode),
                        ],
                        "list_opt_set_piece",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("option set to_json void")?
                    .into_pointer_value();
                // Runtime helper returns a malloc'd C string; register so the
                // sized helper's per-iteration flush frees it.
                self.register_display_alloc(piece);
                Ok(piece)
            },
            "list_opt_set_json",
        )
    }

    /// Map of Map of product-tuple values.
    /// Unified emitter for Map-of-container-of-product JSON serialization.
    /// Replaces 46 near-identical `emit_map_*_product_to_json` functions that
    /// differed only in the runtime function name string.
    fn emit_map_container_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        runtime_fn_name: &str,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let arity = Self::product_tuple_arity(product_type);
        let i64_ty = self.context.i64_type();
        let func = self.get_runtime_fn(runtime_fn_name)?;
        Ok(self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(display_style as u64, false)),
                ],
                "map_container_product_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("map container product to_json void")?
            .into_pointer_value())
    }

    /// Count comma-separated fields at depth 0 inside `(…)` for product tuple arity.
    fn product_tuple_arity(product_type: &str) -> i64 {
        let inner = product_type
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(product_type);
        let mut arity: i64 = 0;
        let mut depth = 0i32;
        let mut any = false;
        for ch in inner.chars() {
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
        // P3-6: empty tuple fallback should be 0, not 2. An empty tuple
        // has no fields; fallback=2 would generate a wrong runtime call.
        // In practice, empty tuples don't reach this path (container product
        // requires starts_with('(') and at least one comma for any=true).
        if arity <= 0 {
            arity = 0;
        }
        arity
    }

    /// Recursively parse a container type string (e.g. `List<Set<Option<(i32,i32)>>>`)
    /// into a runtime function name for product-tuple JSON serialization.
    ///
    /// Returns `Some((runtime_fn_name, resolved_product_type))` if the innermost
    /// type is a product tuple, `None` otherwise (caller falls through to scalar handling).
    ///
    /// The runtime fn name is `{prefix}_{path}_product_i64` where path is the
    /// container types joined by `_` (e.g. `mimi_map_to_json_list_set_option_product_i64`).
    fn resolve_container_product(
        &self,
        value_type: &str,
        prefix: &str,
    ) -> Option<(String, String)> {
        let mut path: Vec<&'static str> = Vec::new();
        self.resolve_container_product_recursive(value_type, prefix, &mut path)
    }

    fn resolve_container_product_recursive(
        &self,
        current: &str,
        prefix: &str,
        path: &mut Vec<&'static str>,
    ) -> Option<(String, String)> {
        // Base case: product tuple
        if current.starts_with('(') || self.is_product_tuple_alias(current) {
            let resolved = if self.is_product_tuple_alias(current) {
                self.resolve_alias_type_name(current)
            } else {
                current.to_string()
            };
            let fn_name = if path.is_empty() {
                format!("{prefix}_product_i64")
            } else {
                format!("{prefix}_{}_product_i64", path.join("_"))
            };
            return Some((fn_name, resolved));
        }

        // Recursive cases: strip one container layer and recurse.
        // Branches are mutually exclusive (a type string starts with at most one prefix).
        if let Some(inner) = current
            .strip_prefix("List<")
            .and_then(|s| s.strip_suffix('>'))
        {
            path.push("list");
            let result = self.resolve_container_product_recursive(inner, prefix, path);
            if result.is_none() {
                path.pop();
            }
            return result;
        }
        if let Some(inner) = current
            .strip_prefix("Map<string, ")
            .and_then(|s| s.strip_suffix('>'))
        {
            path.push("map");
            let result = self.resolve_container_product_recursive(inner, prefix, path);
            if result.is_none() {
                path.pop();
            }
            return result;
        }
        if let Some(inner) = current
            .strip_prefix("Set<")
            .and_then(|s| s.strip_suffix('>'))
        {
            path.push("set");
            let result = self.resolve_container_product_recursive(inner, prefix, path);
            if result.is_none() {
                path.pop();
            }
            return result;
        }
        if let Some(inner) = current
            .strip_prefix("Option<")
            .and_then(|s| s.strip_suffix('>'))
        {
            path.push("option");
            let result = self.resolve_container_product_recursive(inner, prefix, path);
            if result.is_none() {
                path.pop();
            }
            return result;
        }
        // Result<T, E>: extract ok type T via depth-0 comma scan
        if let Some(after) = current.strip_prefix("Result<") {
            let mut depth = 0i32;
            for (i, ch) in after.char_indices() {
                match ch {
                    '<' | '(' => depth += 1,
                    '>' | ')' => depth -= 1,
                    ',' if depth == 0 => {
                        let ok_ty = after[..i].trim();
                        path.push("result");
                        let result = self.resolve_container_product_recursive(ok_ty, prefix, path);
                        if result.is_none() {
                            path.pop();
                        }
                        return result;
                    }
                    _ => {}
                }
            }
        }

        None
    }

    pub(in crate::codegen) fn emit_map_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_set_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_set_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_option_set_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_option_set_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_option_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_option_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_set_list_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_set_list_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_set_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_set_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_map_result_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_map_result_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_map_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_map_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_map_option_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_map_option_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_map_set_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_map_set_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_set_map_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_set_map_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_set_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_set_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_map_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_map_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_map_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_map_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_set_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_set_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_set_option_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_set_option_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_set_result_option_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_set_result_option_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_set_result_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_set_result_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_result_option_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_result_option_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_set_result_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_set_result_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_set_option_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_set_option_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_set_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_set_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_list_result_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_list_result_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_option_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_option_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_result_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_result_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_list_option_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_list_option_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_list_set_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_list_set_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_result_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_result_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_option_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_option_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_set_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_set_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_set_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_set_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_list_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_list_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_list_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_list_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_set_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_set_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_set_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_set_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_set_list_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_set_list_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_set_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_set_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_option_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_option_map_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_result_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_result_product_i64",
            product_type,
            display_style,
        )
    }

    pub(in crate::codegen) fn emit_map_product_to_json(
        &self,
        map_handle: inkwell::values::IntValue<'ctx>,
        product_type: &str,
        display_style: i64,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_map_container_product_to_json(
            map_handle,
            "mimi_map_to_json_product_i64",
            product_type,
            display_style,
        )
    }

    /// JSON for a List payload given its type string `List<…>` (or bare inner).
    /// `list_ptr` points at a list struct `{i64,ptr}`.
    pub(in crate::codegen) fn emit_list_payload_to_json_cstr(
        &self,
        list_ptr: inkwell::values::PointerValue<'ctx>,
        list_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let inner = list_type
            .strip_prefix("List<")
            .and_then(|s| s.strip_suffix('>'))
            .unwrap_or(list_type);
        if inner.starts_with('(') || self.is_product_tuple_alias(inner) {
            let elem = if self.is_product_tuple_alias(inner) {
                self.resolve_alias_type_name(inner)
            } else {
                inner.to_string()
            };
            return self.emit_list_product_tuple_to_json(list_ptr, &elem);
        }
        if let Some(opt_inner) = inner
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
                return Ok(self
                    .build_call(
                        func,
                        &[
                            BasicMetadataValueEnum::PointerValue(list_ptr),
                            BasicMetadataValueEnum::IntValue(i64_ty.const_int(arity as u64, false)),
                            BasicMetadataValueEnum::IntValue(i64_ty.const_int(0, false)),
                        ],
                        "list_opt_prod_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("list option product to_json void")?
                    .into_pointer_value());
            }
        }
        if inner.starts_with("Map") {
            if let Some(val_ty) = inner
                .strip_prefix("Map<string, ")
                .and_then(|s| s.strip_suffix('>'))
                .or_else(|| {
                    inner
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
                    return self.emit_list_map_product_to_json(list_ptr, &elem);
                }
            }
        }
        if inner.starts_with("List<") {
            let mid_elem = Self::strip_first_type_arg(&format!("List<{}>", inner), "List")
                .and_then(|mid| Self::strip_first_type_arg(&mid, "List"))
                .unwrap_or_else(|| inner.to_string());
            if mid_elem.starts_with('(') || self.is_product_tuple_alias(&mid_elem) {
                let elem = if self.is_product_tuple_alias(&mid_elem) {
                    self.resolve_alias_type_name(&mid_elem)
                } else {
                    mid_elem
                };
                return self.emit_list_list_product_tuple_to_json(list_ptr, &elem);
            }
        }
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_name = if list_type.contains("List<string>") || inner == "string" {
            "mimi_list_str_to_json"
        } else if list_type.contains("List<bool>") || inner == "bool" {
            "mimi_list_bool_to_json"
        } else if list_type.contains("List<f64>")
            || list_type.contains("List<f32>")
            || inner == "f64"
            || inner == "f32"
        {
            "mimi_list_f64_to_json"
        } else {
            "mimi_list_i64_to_json"
        };
        let fn_ty = i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
        let list_fn = self.module.get_function(fn_name).unwrap_or_else(|| {
            self.module
                .add_function(fn_name, fn_ty, Some(inkwell::module::Linkage::External))
        });
        Ok(self
            .build_call(
                list_fn,
                &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                "list_payload_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("list to_json void")?
            .into_pointer_value())
    }

    /// Display helper for Result Ok of List — routes product-tuple lists correctly.
    fn emit_result_ok_list_display(
        &self,
        list_sv: inkwell::values::StructValue<'ctx>,
        arg_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        if arg_type.contains("List<string>") {
            return self.emit_list_string_to_string(list_sv);
        }
        if let Some(inner) = Self::strip_first_type_arg(arg_type, "Result")
            .and_then(|s| s.strip_prefix("List<").map(|x| x.to_string()))
            .and_then(|s| s.strip_suffix('>').map(|x| x.to_string()))
        {
            if inner.starts_with('(') || self.is_product_tuple_alias(&inner) {
                let elem = if self.is_product_tuple_alias(&inner) {
                    self.resolve_alias_type_name(&inner)
                } else {
                    inner
                };
                return self.emit_list_product_tuple_to_string(list_sv, &elem);
            }
            if let Some(opt_inner) = inner
                .strip_prefix("Option<")
                .and_then(|s| s.strip_suffix('>'))
            {
                if opt_inner.starts_with('(') || self.is_product_tuple_alias(opt_inner) {
                    let elem = if self.is_product_tuple_alias(opt_inner) {
                        self.resolve_alias_type_name(opt_inner)
                    } else {
                        opt_inner.to_string()
                    };
                    // list_sv is list struct; store and pass pointer to runtime.
                    let list_ty = self.list_struct_type();
                    let alloca =
                        self.build_alloca(BasicTypeEnum::StructType(list_ty), "res_ok_list_opt")?;
                    self.build_store(alloca, list_sv)?;
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
                    return Ok(self
                        .build_call(
                            func,
                            &[
                                BasicMetadataValueEnum::PointerValue(alloca),
                                BasicMetadataValueEnum::IntValue(
                                    i64_ty.const_int(arity as u64, false),
                                ),
                                BasicMetadataValueEnum::IntValue(i64_ty.const_int(1, false)),
                            ],
                            "res_ok_list_opt_disp",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("list option product display void")?
                        .into_pointer_value());
                }
            }
            if inner.starts_with("Map") {
                return self.emit_list_map_to_string(list_sv, &inner);
            }
            if inner.starts_with("Set") {
                return self.emit_list_set_to_string(list_sv, &inner);
            }
        }
        self.emit_list_i32_to_string(list_sv)
    }

    /// Format a heterogeneous product/tuple (ints, bools, strings, nested structs).
    /// 0.35.20 (#6): extract the `idx`-th field type of a product-tuple type
    /// string like `(List<i32>, (i32, string))`, honoring nested brackets and
    /// parens. Returns `None` when `ts` is not a tuple or `idx` is out of
    /// range.
    fn tuple_field_type(ts: &str, idx: usize) -> Option<String> {
        let ts = ts.trim();
        if !ts.starts_with('(') || !ts.ends_with(')') {
            return None;
        }
        let inner = &ts[1..ts.len() - 1];
        let mut depth = 0i32;
        let mut start = 0;
        let mut cur = 0;
        for (i, ch) in inner.char_indices() {
            match ch {
                '<' | '(' => depth += 1,
                '>' | ')' => depth -= 1,
                ',' if depth == 0 => {
                    if cur == idx {
                        return Some(inner[start..i].trim().to_string());
                    }
                    cur += 1;
                    start = i + 1;
                }
                _ => {}
            }
        }
        if cur == idx {
            Some(inner[start..].trim().to_string())
        } else {
            None
        }
    }

    fn emit_product_tuple_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        type_str: Option<&str>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        // Wave-1 audit fix (§8, FIX: fixed 4096-byte strcat assembly):
        // exact-size assembly — one piece per field, computed lengths, one
        // malloc. Also removes the per-field 256-byte piece buffers that
        // truncated long string fields.
        let fields = sv.get_type().get_field_types();
        let i64_ty = self.context.i64_type();
        let mut parts: Vec<CatPart<'ctx>> = Vec::with_capacity(fields.len() * 2 + 2);
        parts.push(CatPart::Lit("("));
        for (i, ft) in fields.iter().enumerate() {
            if i > 0 {
                parts.push(CatPart::Lit(", "));
            }
            let field_val =
                self.build_extract_value(sv.into(), i as u32, &format!("prod_tup_{}", i))?;
            match (ft, field_val) {
                (BasicTypeEnum::IntType(it), BasicValueEnum::IntValue(iv)) => {
                    if it.get_bit_width() == 1 {
                        let true_g = self
                            .builder
                            .build_global_string_ptr("true", "prod_true")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let false_g = self
                            .builder
                            .build_global_string_ptr("false", "prod_false")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let zero = iv.get_type().const_int(0, false);
                        let is_t = self
                            .builder
                            .build_int_compare(IntPredicate::NE, iv, zero, "prod_bool")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let sel = self
                            .builder
                            .build_select(
                                is_t,
                                true_g.as_pointer_value(),
                                false_g.as_pointer_value(),
                                "prod_bool_s",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        // Static globals — no display-free registration.
                        parts.push(CatPart::Dyn(sel.into_pointer_value()));
                    } else {
                        let as_i64 = if iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(iv, i64_ty, "prod_sext")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            iv
                        };
                        let to_i64_fn = self.get_runtime_fn("mimi_to_string_i64")?;
                        let s = self
                            .build_call(
                                to_i64_fn,
                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                "prod_i64_str",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_to_string_i64 returned void")?
                            .into_pointer_value();
                        self.register_display_alloc(s);
                        parts.push(CatPart::Dyn(s));
                    }
                }
                (BasicTypeEnum::StructType(sty), BasicValueEnum::StructValue(fsv)) => {
                    let ffields = sty.get_field_types();
                    if ffields.len() >= 1 && matches!(ffields[0], BasicTypeEnum::PointerType(_)) {
                        // string {ptr,len}: codegen strings are NUL-terminated
                        // C strings, so the data pointer can feed strlen/memcpy
                        // directly (no 256-byte truncation anymore).
                        let ptr = self
                            .build_extract_value(fsv.into(), 0, "prod_str_ptr")?
                            .into_pointer_value();
                        parts.push(CatPart::Dyn(ptr));
                    } else if ffields.len() == 2
                        && matches!(
                            ffields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        )
                        && matches!(ffields[1], BasicTypeEnum::PointerType(_))
                    {
                        // 0.35.20 (#6): Mimi list struct {i64 len, ptr data}
                        // by-value inside a product tuple (e.g. partition's
                        // `(List<T>, List<T>)` or a user function returning
                        // `(List<i32>, List<i32>)`). The old fallback recursed
                        // into the product formatter and printed `(len, ptr)`
                        // garbage. Dispatch on the field's List<...> type when
                        // the tuple type string is available.
                        let field_ty = type_str.and_then(|ts| Self::tuple_field_type(ts, i));
                        let list_str = match field_ty {
                            Some(ft) => self.emit_list_typed_to_string(fsv, &ft)?,
                            None => self.emit_list_i32_to_string(fsv)?,
                        };
                        parts.push(CatPart::Dyn(list_str));
                    } else {
                        // Nested product — recurse with the sub-field type.
                        let sub = type_str.and_then(|ts| Self::tuple_field_type(ts, i));
                        let nested = self.emit_product_tuple_to_string(fsv, sub.as_deref())?;
                        parts.push(CatPart::Dyn(nested));
                    }
                }
                (BasicTypeEnum::PointerType(_), BasicValueEnum::PointerValue(pv)) => {
                    parts.push(CatPart::Dyn(pv));
                }
                (BasicTypeEnum::FloatType(_), BasicValueEnum::FloatValue(fv)) => {
                    // Shortest round-trip Display (VM parity) instead of %g's
                    // 6 significant digits.
                    let to_f64_fn = self.get_runtime_fn("mimi_to_string_f64")?;
                    let s = self
                        .build_call(
                            to_f64_fn,
                            &[BasicMetadataValueEnum::FloatValue(fv)],
                            "prod_f64_str",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("mimi_to_string_f64 returned void")?
                        .into_pointer_value();
                    self.register_display_alloc(s);
                    parts.push(CatPart::Dyn(s));
                }
                _ => {
                    parts.push(CatPart::Lit("?"));
                }
            }
        }
        parts.push(CatPart::Lit(")"));
        self.sized_cat_parts(&parts, "prod_tup", true)
    }

    /// Serialize a product-tuple struct to a JSON array C string (compact,
    /// matching serde_json / interp `to_json` for `Value::Tuple`).
    ///
    /// D-4 (2026-08-06): fixed 4096-byte strcat assembly replaced by exact-size
    /// two-pass `sized_cat_parts`. Field pieces are rendered once per pass (the
    /// renderers are pure), registered as display allocs and freed by
    /// `flush_display_since(marker)` right after assembly — the returned buffer
    /// itself is unregistered (JSON producers register it themselves, e.g.
    /// `register_heap_alloc` at the call site).
    pub(in crate::codegen) fn emit_product_tuple_to_json(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let fields = sv.get_type().get_field_types();
        let i64_ty = self.context.i64_type();
        let snprintf_fn = self.get_runtime_fn("snprintf")?;
        let marker = self.display_marker();
        let mut parts: Vec<CatPart<'ctx>> = Vec::new();
        parts.push(CatPart::Lit("["));
        // Snprintf scratch for int/float pieces. %ld max length is i64::MIN's
        // 20 chars + NUL = 21; %g for a double uses at most ~15 + NUL. 32 B
        // covers both with room to spare.
        let buf_size = i64_ty.const_int(32, false);
        for (i, ft) in fields.iter().enumerate() {
            if i > 0 {
                parts.push(CatPart::Lit(","));
            }
            let field_val =
                self.build_extract_value(sv.into(), i as u32, &format!("json_tup_{}", i))?;
            match (ft, field_val) {
                (BasicTypeEnum::IntType(it), BasicValueEnum::IntValue(iv)) => {
                    if it.get_bit_width() == 1 {
                        let true_g = self
                            .builder
                            .build_global_string_ptr("true", "json_tup_true")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let false_g = self
                            .builder
                            .build_global_string_ptr("false", "json_tup_false")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let zero = iv.get_type().const_int(0, false);
                        let is_t = self
                            .builder
                            .build_int_compare(IntPredicate::NE, iv, zero, "json_tup_bool")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let sel = self
                            .builder
                            .build_select(
                                is_t,
                                true_g.as_pointer_value(),
                                false_g.as_pointer_value(),
                                "json_tup_bool_s",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        // Static global string — do not register.
                        parts.push(CatPart::Dyn(sel.into_pointer_value()));
                    } else {
                        let as_i64 = if iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(iv, i64_ty, "json_tup_sext")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            iv
                        };
                        let fmt = self
                            .builder
                            .build_global_string_ptr("%ld", "json_tup_ld")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let piece = self.malloc_or_abort(buf_size, "json_tup_piece")?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(piece),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(as_i64),
                            ],
                            "json_tup_ld_sn",
                        )?;
                        self.register_display_alloc(piece);
                        parts.push(CatPart::Dyn(piece));
                    }
                }
                (BasicTypeEnum::StructType(sty), BasicValueEnum::StructValue(fsv)) => {
                    let ffields = sty.get_field_types();
                    if ffields.len() == 2
                        && matches!(ffields[0], BasicTypeEnum::PointerType(_))
                        && matches!(
                            ffields[1],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                        )
                    {
                        // string {ptr,len} → JSON-escaped quoted string
                        // (mimi_json_escape_string already wraps with ").
                        let ptr = self
                            .build_extract_value(fsv.into(), 0, "json_tup_str_ptr")?
                            .into_pointer_value();
                        let esc_fn = self.get_runtime_fn("mimi_json_escape_string")?;
                        let escaped = self
                            .build_call(
                                esc_fn,
                                &[BasicMetadataValueEnum::PointerValue(ptr)],
                                "json_tup_esc",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_json_escape_string void")?
                            .into_pointer_value();
                        self.register_display_alloc(escaped);
                        parts.push(CatPart::Dyn(escaped));
                    } else {
                        // Nested product tuple (recursion returns an
                        // unregistered buffer; register so the outer marker
                        // flush releases it).
                        let nested = self.emit_product_tuple_to_json(fsv)?;
                        self.register_display_alloc(nested);
                        parts.push(CatPart::Dyn(nested));
                    }
                }
                (BasicTypeEnum::FloatType(_), BasicValueEnum::FloatValue(fv)) => {
                    let fmt = self
                        .builder
                        .build_global_string_ptr("%g", "json_tup_f")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let piece = self.malloc_or_abort(buf_size, "json_tup_piece")?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(piece),
                            BasicMetadataValueEnum::IntValue(buf_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::FloatValue(fv),
                        ],
                        "json_tup_f_sn",
                    )?;
                    self.register_display_alloc(piece);
                    parts.push(CatPart::Dyn(piece));
                }
                _ => {
                    parts.push(CatPart::Lit("null"));
                }
            }
        }
        parts.push(CatPart::Lit("]"));
        let buf = self.sized_cat_parts(&parts, "json_tup", false)?;
        // Free the per-field scratch pieces (nested JSON buffers, escaped
        // strings, snprintf buffers) now that they are consumed. The main
        // buffer is deliberately left unregistered for the caller.
        self.flush_display_since(marker)?;
        Ok(buf)
    }

    /// Format an all-integer struct (tuple / map_get) as `(v0, v1, ...)`.
    fn emit_int_tuple_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let fields = sv.get_type().get_field_types();
        let i64_ty = self.context.i64_type();
        let mut fmt = String::from("(");
        let mut sprintf_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for (i, ft) in fields.iter().enumerate() {
            if i > 0 {
                fmt.push_str(", ");
            }
            let field_val = self.build_extract_value(sv.into(), i as u32, &format!("tup_{}", i))?;
            let iv = field_val.into_int_value();
            let bw = iv.get_type().get_bit_width();
            // Bool (i1/i8 disc-like small ints used as flags): print true/false
            // only when bit-width is 1; i32/i64 stay numeric.
            if bw == 1 {
                fmt.push_str("%s");
                let true_g = self
                    .builder
                    .build_global_string_ptr("true", "tup_true")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let false_g = self
                    .builder
                    .build_global_string_ptr("false", "tup_false")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let zero = iv.get_type().const_int(0, false);
                let is_true = self
                    .builder
                    .build_int_compare(IntPredicate::NE, iv, zero, "tup_bool")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let selected = self
                    .builder
                    .build_select(
                        is_true,
                        true_g.as_pointer_value(),
                        false_g.as_pointer_value(),
                        "tup_bool_str",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                sprintf_args.push(BasicMetadataValueEnum::PointerValue(
                    selected.into_pointer_value(),
                ));
            } else {
                fmt.push_str("%ld");
                let ext = if bw < 64 {
                    // i32 found flags from map_has_key: treat 0/1 as bool-like for display
                    // when field is i32 and value is 0 or 1? Keep numeric for generality.
                    self.builder
                        .build_int_s_extend(iv, i64_ty, "tup_sext")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    iv
                };
                sprintf_args.push(BasicMetadataValueEnum::IntValue(ext));
            }
            let _ = ft;
        }
        fmt.push(')');
        let est = (fmt.len() + fields.len() * 24 + 64) as u64;
        let buf_size = i64_ty.const_int(est, false);
        let buf = self.malloc_display_buf(buf_size, "tup_print_buf")?;
        let fmt_ptr = self
            .builder
            .build_global_string_ptr(&fmt, "tup_print_fmt")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let mut all_args = vec![
            BasicMetadataValueEnum::PointerValue(buf),
            BasicMetadataValueEnum::IntValue(buf_size),
            BasicMetadataValueEnum::PointerValue(fmt_ptr.as_pointer_value()),
        ];
        all_args.extend(sprintf_args);
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module
                .add_function("snprintf", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(snprintf_fn, &all_args, "tup_snprintf")?;
        Ok(buf)
    }

    /// Materialize a list struct value into an alloca and call the runtime
    /// helper `mimi_list_i32_to_string` to get a printable C string.
    fn emit_list_i32_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let list_struct_ty = self.list_struct_type();
        let alloca = self.build_alloca(list_struct_ty, "print_list_alloca")?;
        self.build_store(alloca, sv)?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
        let callee = self
            .module
            .get_function("mimi_list_i32_to_string")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_list_i32_to_string",
                    fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let raw = self
            .build_call(
                callee,
                &[BasicMetadataValueEnum::PointerValue(alloca)],
                "list_i32_to_str",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_list_i32_to_string returned void")?
            .into_pointer_value();
        // Q2: the runtime helper allocates a C string consumed by the
        // print call — register it for release at flush_display_frees.
        self.register_display_alloc(raw);
        Ok(raw)
    }

    /// Wave-1 audit fix (§8, FIX: list display i32 fallback misread element
    /// types): render `List<f64>` / `List<i64>` / `List<bool>` with
    /// element-kind-correct formatting. Scalar list slots are i64-sized; f64
    /// elements are stored there as bitcast bit patterns (see
    /// `coerce_to_list_storage`), so we load the i64 slot and interpret it
    /// per kind instead of truncating to i32. Sized two-pass assembly — no
    /// fixed buffer.
    fn emit_list_scalar_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        kind: ScalarListKind,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let len = self
            .build_extract_value(sv.into(), 0, "list_scalar_len")?
            .into_int_value();
        let data_ptr = self
            .build_extract_value(sv.into(), 1, "list_scalar_data")?
            .into_pointer_value();
        let name = match kind {
            ScalarListKind::F64 => "list_f64",
            ScalarListKind::I64 => "list_i64",
            ScalarListKind::Bool => "list_bool",
        };
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // slots); the two-pass loop guard `idx ULT len` gates this.
                let elem_slot = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], "list_scalar_slot")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let raw = self
                    .build_load(i64_ty, elem_slot, "list_scalar_elem")?
                    .into_int_value();
                match kind {
                    ScalarListKind::F64 => {
                        let f64_ty = self.context.f64_type();
                        let fv = self
                            .build_bit_cast(
                                raw.into(),
                                BasicTypeEnum::FloatType(f64_ty),
                                "list_scalar_f64",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            .into_float_value();
                        let to_f64_fn = self.get_runtime_fn("mimi_to_string_f64")?;
                        let s = self
                            .build_call(
                                to_f64_fn,
                                &[BasicMetadataValueEnum::FloatValue(fv)],
                                "list_scalar_f64_str",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_to_string_f64 returned void")?
                            .into_pointer_value();
                        self.register_display_alloc(s);
                        Ok(s)
                    }
                    ScalarListKind::I64 => {
                        let to_i64_fn = self.get_runtime_fn("mimi_to_string_i64")?;
                        let s = self
                            .build_call(
                                to_i64_fn,
                                &[BasicMetadataValueEnum::IntValue(raw)],
                                "list_scalar_i64_str",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_to_string_i64 returned void")?
                            .into_pointer_value();
                        self.register_display_alloc(s);
                        Ok(s)
                    }
                    ScalarListKind::Bool => {
                        let true_g = self
                            .builder
                            .build_global_string_ptr("true", "list_scalar_true")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let false_g = self
                            .builder
                            .build_global_string_ptr("false", "list_scalar_false")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let is_true = self
                            .builder
                            .build_int_compare(
                                IntPredicate::NE,
                                raw,
                                i64_ty.const_int(0, false),
                                "list_scalar_bool",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        // Static globals — not registered for display free.
                        let sel = self
                            .builder
                            .build_select(
                                is_true,
                                true_g.as_pointer_value(),
                                false_g.as_pointer_value(),
                                "list_scalar_bool_s",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        Ok(sel.into_pointer_value())
                    }
                }
            },
            name,
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Format `List<List<(…)>>` by reconstructing each inner list of product tuples.
    pub(in crate::codegen) fn emit_list_list_product_tuple_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        elem_type_str: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca =
            self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_list_tup_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        // Wave-1 audit fix (§8, FIX: fixed 8192-byte strcat assembly — deeply
        // nested list-of-list-of-tuples rendering beyond 8KB overflowed the
        // display buffer): exact-size two-pass assembly via the sized helper.
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                let data_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 1, "list_list_tup_data_gep")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let data_ptr = self
                    .build_load(i8_ptr, data_gep, "list_list_tup_data")?
                    .into_pointer_value();
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // slots, loaded from the struct's data field). The sized
                // helper's loop guards `idx ULT len` gate every call.
                let elem_slot = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], "list_list_tup_slot")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let elem_i64 = self
                    .build_load(i64_ty, elem_slot, "list_list_tup_elem")?
                    .into_int_value();
                let inner_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_list_tup_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let inner_sv = self
                    .build_load(
                        BasicTypeEnum::StructType(list_ty),
                        inner_ptr,
                        "list_list_tup_ld",
                    )?
                    .into_struct_value();
                self.emit_list_product_tuple_to_string(inner_sv, elem_type_str)
            },
            "list_list_tup",
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Audit wave2 (D-5b): display `List<List<f64>>` / `List<List<i64>>` /
    /// `List<List<bool>>` — outer sized assembly, inner lists rendered by
    /// the element-kind-correct `emit_list_scalar_to_string`. Replaces the
    /// old route through `mimi_list_i32_to_string`, which printed f64 bit
    /// patterns as i32 garbage (VM reference: Value::List Display per
    /// element type).
    fn emit_list_list_scalar_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        kind: ScalarListKind,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let name = match kind {
            ScalarListKind::F64 => "list_list_f64",
            ScalarListKind::I64 => "list_list_i64",
            ScalarListKind::Bool => "list_list_bool",
        };
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), name)?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        let buf = self.emit_sized_list_of_pieces(
            len,
            ", ",
            |idx| {
                let data_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 1, &format!("{}_data_gep", name))
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let data_ptr = self
                    .build_load(i8_ptr, data_gep, &format!("{}_data", name))?
                    .into_pointer_value();
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // slots, loaded from the struct's data field). The sized
                // helper's loop guards `idx ULT len` gate every call.
                let elem_slot = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], &format!("{}_slot", name))
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let elem_i64 = self
                    .build_load(i64_ty, elem_slot, &format!("{}_elem", name))?
                    .into_int_value();
                let inner_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, &format!("{}_as_ptr", name))
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let inner_sv = self
                    .build_load(
                        BasicTypeEnum::StructType(list_ty),
                        inner_ptr,
                        &format!("{}_ld", name),
                    )?
                    .into_struct_value();
                self.emit_list_scalar_to_string(inner_sv, kind)
            },
            name,
        )?;
        self.register_display_alloc(buf);
        Ok(buf)
    }

    /// Serialize `List<List<(…)>>` to nested JSON arrays of product tuples.
    pub(in crate::codegen) fn emit_list_list_product_tuple_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        elem_type_str: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_list_tup_json_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_list_tup_json_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        // D-4: exact-size two-pass assembly instead of the fixed 8192-byte
        // strcat loop. Elements are inner list pointers (no branching), so
        // each piece is defined unconditionally and the sized helper's flush
        // frees it exactly once per iteration (the old loop leaked every
        // element's nested list JSON).
        self.emit_sized_list_of_pieces(
            len,
            ",",
            |idx| {
                // SAFETY: data_ptr is the collection's data array (`len` i64
                // elements); the sized helper's loop guard gates idx < len.
                let elem_slot = unsafe {
                    self.builder
                        .build_gep(i64_ty, data_ptr, &[idx], "list_list_tup_json_slot")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                };
                let elem_i64 = self
                    .builder
                    .build_load(i64_ty, elem_slot, "list_list_tup_json_elem")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_int_value();
                let inner_ptr = self
                    .builder
                    .build_int_to_ptr(elem_i64, i8_ptr, "list_list_tup_json_as_ptr")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                // Inner list is stored as ptrtoint of list struct.
                // emit_list_product_tuple_to_json returns an unregistered
                // buffer; register so the sized helper frees it.
                let piece = self.emit_list_product_tuple_to_json(inner_ptr, elem_type_str)?;
                self.register_display_alloc(piece);
                Ok(piece)
            },
            "list_list_tup_json",
        )
    }

    /// Materialize a `List<List<T>>` struct value into an alloca and call
    /// `mimi_list_list_to_string` with the appropriate inner-list formatter.
    fn emit_list_list_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        inner_fn_name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let list_struct_ty = self.list_struct_type();
        let alloca = self.build_alloca(list_struct_ty, "print_list_list_alloca")?;
        self.build_store(alloca, sv)?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let callback_fn_ty =
            i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
        let inner_fn = self.module.get_function(inner_fn_name).unwrap_or_else(|| {
            self.module.add_function(
                inner_fn_name,
                callback_fn_ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        let callback = inner_fn.as_global_value().as_pointer_value();
        let fn_ty = i8_ptr_ty.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
            ],
            false,
        );
        let callee = self
            .module
            .get_function("mimi_list_list_to_string")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_list_list_to_string",
                    fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let raw = self
            .build_call(
                callee,
                &[
                    BasicMetadataValueEnum::PointerValue(alloca),
                    BasicMetadataValueEnum::PointerValue(callback),
                ],
                "list_list_to_str",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_list_list_to_string returned void")?
            .into_pointer_value();
        Ok(raw)
    }

    /// Materialize a list struct value into an alloca and call the runtime
    /// helper `mimi_list_to_string` to get a printable C string for string lists.
    fn emit_list_string_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let list_struct_ty = self.list_struct_type();
        let alloca = self.build_alloca(list_struct_ty, "print_str_list_alloca")?;
        self.build_store(alloca, sv)?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
        let callee = self
            .module
            .get_function("mimi_list_to_string")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_list_to_string",
                    fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let raw = self
            .build_call(
                callee,
                &[BasicMetadataValueEnum::PointerValue(alloca)],
                "list_to_str",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_list_to_string returned void")?
            .into_pointer_value();
        // Q2: register the runtime-allocated C string for display release.
        self.register_display_alloc(raw);
        Ok(raw)
    }

    pub(super) fn compile_print(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // VM parity: print() prints nothing; print(a, b, ...) prints all
        // arguments separated by a single space.
        let i64_ty = self.context.i64_type();
        if args.is_empty() {
            return Ok(i64_ty.const_int(0, false).into());
        }
        let arg_types: Vec<String> = self.pending_print_arg_types.clone();
        let mut print_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        let mut fmt_str = String::new();
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                fmt_str.push(' ');
            }
            let arg_type = arg_types.get(i).cloned().unwrap_or_default();
            let (print_arg, spec) = self.extract_print_arg(arg, i64_ty, &arg_type)?;
            print_args.push(print_arg);
            fmt_str.push_str(&spec);
        }
        let fmt_global = self
            .builder
            .build_global_string_ptr(&fmt_str, "print_fmt")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        let mut printf_args = vec![BasicMetadataValueEnum::PointerValue(
            fmt_global.as_pointer_value(),
        )];
        printf_args.extend(print_args);
        let printf = self.get_runtime_fn("printf")?;
        self.build_call(printf, &printf_args, "print_call")?;
        self.flush_display_frees()?;
        Ok(i64_ty.const_int(0, false).into())
    }

    pub(super) fn compile_eprintln(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // VM parity: eprintln() prints a newline to stderr; eprintln(a, b, ...)
        // prints all arguments separated by a single space plus a newline.
        let i64_ty = self.context.i64_type();
        let mut fmt_str = String::new();
        let mut print_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        if !args.is_empty() {
            let arg_types: Vec<String> = self.pending_print_arg_types.clone();
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    fmt_str.push(' ');
                }
                let arg_type = arg_types.get(i).cloned().unwrap_or_default();
                let (print_arg, spec) = self.extract_print_arg(arg, i64_ty, &arg_type)?;
                print_args.push(print_arg);
                fmt_str.push_str(&spec);
            }
        }
        fmt_str.push('\n');
        let fmt_global = self
            .builder
            .build_global_string_ptr(&fmt_str, "efmt")
            .map_err(|e| CompileError::LlvmError(format!("efmt error: {}", e)))?;
        // stderr, not stdout (Wave-1 audit fix §8).
        let stderr_stream = self.get_stream_global("stderr")?;
        let fprintf = self
            .module
            .get_function("fprintf")
            .ok_or_else(|| "fprintf not declared".to_string())?;
        let mut fprintf_args = vec![
            BasicMetadataValueEnum::PointerValue(stderr_stream),
            BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
        ];
        fprintf_args.extend(print_args);
        self.build_call(fprintf, &fprintf_args, "eprintf_call")?;
        self.flush_display_frees()?;
        Ok(i64_ty.const_int(0, false).into())
    }

    pub(super) fn compile_assert(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() || args.len() > 2 {
            return Err(CompileError::WrongArgCount(
                "assert expects 1 or 2 arguments (condition, optional message)".to_string(),
            ));
        }
        let cond_raw = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "assert requires boolean/i64 argument".to_string(),
                ))
            }
        };
        // H-22 (audit 2026-08-05-0656, io.rs half): builtin predicates
        // (`str_contains`, `contains`, `has_key`, …) return i64 booleans;
        // `br` requires i1. Normalize non-i1 conditions with a zero compare
        // — mirrors the if-stmt/while fix in block.rs (the zero constant
        // matches the condition's own width; icmp operands must agree).
        let cond = if cond_raw.get_type().get_bit_width() == 1 {
            cond_raw
        } else {
            let zero = cond_raw.get_type().const_zero();
            self.builder
                .build_int_compare(IntPredicate::NE, cond_raw, zero, "assert_cond_to_i1")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "assert_ok");
        let fail_bb = self.context.append_basic_block(function, "assert_fail");
        self.build_cond_br(cond, ok_bb, fail_bb)?;

        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        if args.len() == 2 {
            // Use custom message
            let msg_ptr = self.extract_raw_str_ptr(&args[1]).map_err(|_| {
                CompileError::TypeMismatch(
                    "assert message argument must be a string pointer".to_string(),
                )
            })?;
            // Wave-1 audit fix (§8, FIX: user message passed AS printf format
            // string — format-string UB for messages containing '%' specs,
            // e.g. `assert(false, "100% done")`). The message is DATA: print
            // it through a literal "%s" format, same destination (stdout) as
            // the legacy path.
            let msg_fmt = self
                .builder
                .build_global_string_ptr("%s", "assert_msg_fmt")
                .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
            self.build_call(
                printf,
                &[
                    BasicMetadataValueEnum::PointerValue(msg_fmt.as_pointer_value()),
                    BasicMetadataValueEnum::PointerValue(msg_ptr),
                ],
                "assert_printf",
            )?;
        } else {
            let fmt_global = self
                .builder
                .build_global_string_ptr("assertion failed\n", "assert_msg")
                .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
            self.build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(
                    fmt_global.as_pointer_value(),
                )],
                "assert_printf",
            )?;
        }
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.build_call(
            exit_fn,
            &[BasicMetadataValueEnum::IntValue(
                self.context.i32_type().const_int(1, false),
            )],
            "assert_exit",
        )?;
        // SAFETY: exit(1) is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert_eq(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "assert_eq expects 2 arguments".to_string(),
            ));
        }
        let a = args[0];
        let b = args[1];
        let eq = match (a, b) {
            (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => self
                .builder
                .build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                let strcmp_fn = self
                    .module
                    .get_function("strcmp")
                    .ok_or_else(|| "strcmp not declared".to_string())?;
                let cmp_result = self
                    .builder
                    .build_call(
                        strcmp_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ],
                        "strcmp_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strcmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                self.builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        cmp_result.into_int_value(),
                        zero,
                        "streq",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
            }
            _ => {
                let l_ptr = self.extract_raw_str_ptr(&a).ok();
                let r_ptr = self.extract_raw_str_ptr(&b).ok();
                if let (Some(l), Some(r)) = (l_ptr, r_ptr) {
                    let strcmp_fn = self
                        .module
                        .get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let cmp_result = self
                        .builder
                        .build_call(
                            strcmp_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(l),
                                BasicMetadataValueEnum::PointerValue(r),
                            ],
                            "strcmp_call",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("strcmp returned void")?;
                    let zero = self.context.i32_type().const_int(0, false);
                    self.builder
                        .build_int_compare(
                            inkwell::IntPredicate::EQ,
                            cmp_result.into_int_value(),
                            zero,
                            "streq",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                } else {
                    return Err(CompileError::TypeMismatch(
                        "assert_eq requires same types".to_string(),
                    ));
                }
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert_eq".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "aeq_ok");
        let fail_bb = self.context.append_basic_block(function, "aeq_fail");
        self.build_cond_br(eq, ok_bb, fail_bb)?;

        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;

        // Print "assertion failed: "
        let prefix = self
            .builder
            .build_global_string_ptr("assertion failed: ", "aeq_prefix")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(
                prefix.as_pointer_value(),
            )],
            "aeq_prefix_call",
        )?;

        // Print left value
        self.build_print_value(printf, &a)?;
        // Print " != "
        let sep = self
            .builder
            .build_global_string_ptr(" != ", "aeq_sep")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(sep.as_pointer_value())],
            "aeq_sep_call",
        )?;
        // Print right value
        self.build_print_value(printf, &b)?;
        // Print newline
        let nl = self
            .builder
            .build_global_string_ptr("\n", "aeq_nl")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(nl.as_pointer_value())],
            "aeq_nl_call",
        )?;

        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.build_call(
            exit_fn,
            &[BasicMetadataValueEnum::IntValue(
                self.context.i32_type().const_int(1, false),
            )],
            "aeq_exit",
        )?;
        // SAFETY: exit(1) is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert_ne(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "assert_ne expects 2 arguments".to_string(),
            ));
        }
        let a = args[0];
        let b = args[1];
        let ne = match (a, b) {
            (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => self
                .builder
                .build_int_compare(inkwell::IntPredicate::NE, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => self
                .builder
                .build_float_compare(inkwell::FloatPredicate::ONE, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                let strcmp_fn = self
                    .module
                    .get_function("strcmp")
                    .ok_or_else(|| "strcmp not declared".to_string())?;
                let cmp_result = self
                    .builder
                    .build_call(
                        strcmp_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ],
                        "strcmp_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strcmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                self.builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        cmp_result.into_int_value(),
                        zero,
                        "strne",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
            }
            _ => {
                let l_ptr = self.extract_raw_str_ptr(&a).ok();
                let r_ptr = self.extract_raw_str_ptr(&b).ok();
                if let (Some(l), Some(r)) = (l_ptr, r_ptr) {
                    let strcmp_fn = self
                        .module
                        .get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let cmp_result = self
                        .builder
                        .build_call(
                            strcmp_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(l),
                                BasicMetadataValueEnum::PointerValue(r),
                            ],
                            "strcmp_call",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("strcmp returned void")?;
                    let zero = self.context.i32_type().const_int(0, false);
                    self.builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            cmp_result.into_int_value(),
                            zero,
                            "strne",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                } else {
                    return Err(CompileError::TypeMismatch(
                        "assert_ne requires same types".to_string(),
                    ));
                }
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert_ne".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "ane_ok");
        let fail_bb = self.context.append_basic_block(function, "ane_fail");
        self.build_cond_br(ne, ok_bb, fail_bb)?;

        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        // Print "assertion failed: "
        let prefix = self
            .builder
            .build_global_string_ptr("assertion failed: ", "ane_prefix")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(
                prefix.as_pointer_value(),
            )],
            "ane_prefix_call",
        )?;
        // Print left value
        self.build_print_value(printf, &a)?;
        // Print " == "
        let sep = self
            .builder
            .build_global_string_ptr(" == ", "ane_sep")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(sep.as_pointer_value())],
            "ane_sep_call",
        )?;
        // Print right value
        self.build_print_value(printf, &b)?;
        // Print newline
        let nl = self
            .builder
            .build_global_string_ptr("\n", "ane_nl")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(nl.as_pointer_value())],
            "ane_nl_call",
        )?;
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.build_call(
            exit_fn,
            &[BasicMetadataValueEnum::IntValue(
                self.context.i32_type().const_int(1, false),
            )],
            "ane_exit",
        )?;
        // SAFETY: exit(1) is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert_approx_eq(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "assert_approx_eq expects 2 arguments".to_string(),
            ));
        }
        let a = args[0];
        let b = args[1];
        let eq = match (a, b) {
            (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                let diff = self
                    .builder
                    .build_float_sub(l, r, "diff")
                    .map_err(|e| CompileError::LlvmError(format!("fsub error: {}", e)))?;
                let fabs_fn = self.module.get_function("fabs").unwrap_or_else(|| {
                    let f64 = self.context.f64_type();
                    let ty = f64.fn_type(
                        &[inkwell::types::BasicMetadataTypeEnum::FloatType(f64)],
                        false,
                    );
                    self.module
                        .add_function("fabs", ty, Some(inkwell::module::Linkage::External))
                });
                let abs_diff = self
                    .builder
                    .build_call(
                        fabs_fn,
                        &[BasicMetadataValueEnum::FloatValue(diff)],
                        "fabs_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("fabs error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("fabs returned void")?
                    .into_float_value();
                let eps = self.context.f64_type().const_float(1e-6);
                self.builder
                    .build_float_compare(inkwell::FloatPredicate::OLT, abs_diff, eps, "approx")
                    .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?
            }
            _ => {
                return Err(CompileError::TypeMismatch(
                    "assert_approx_eq requires same numeric types".to_string(),
                ))
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert_approx_eq".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "aaeq_ok");
        let fail_bb = self.context.append_basic_block(function, "aaeq_fail");
        self.build_cond_br(eq, ok_bb, fail_bb)?;
        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        // Print "assertion failed: "
        let prefix = self
            .builder
            .build_global_string_ptr("assertion failed: ", "aaeq_prefix")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(
                prefix.as_pointer_value(),
            )],
            "aaeq_prefix_call",
        )?;
        // Print left value
        self.build_print_value(printf, &a)?;
        // Print " !≈ "
        let sep = self
            .builder
            .build_global_string_ptr(" !≈ ", "aaeq_sep")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(sep.as_pointer_value())],
            "aaeq_sep_call",
        )?;
        // Print right value
        self.build_print_value(printf, &b)?;
        // Print newline
        let nl = self
            .builder
            .build_global_string_ptr("\n", "aaeq_nl")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(nl.as_pointer_value())],
            "aaeq_nl_call",
        )?;
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.build_call(
            exit_fn,
            &[BasicMetadataValueEnum::IntValue(
                self.context.i32_type().const_int(1, false),
            )],
            "aaeq_exit",
        )?;
        // SAFETY: exit(1) is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_input(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() > 1 {
            return Err(CompileError::WrongArgCount(
                "input expects 0 or 1 argument".to_string(),
            ));
        }
        // §8-#86 (fixed): three-side `input()` shape is now aligned on
        // `string` — checker types it `string` (infer/call/simple.rs:572),
        // the bytecode VM returns a bare `Value::String` (trimmed, io.rs
        // builtin_input_line), and codegen returns the {ptr,len} string
        // with the trailing newline trimmed. On EOF/error codegen returns
        // an EMPTY string (deterministic, matching VM's empty read).
        //
        // Batch4-01 P2-7: the old implementation used a fixed 4096-byte
        // fgets buffer and truncated long input lines. The runtime helper
        // reads with Rust's unbounded read_line and applies the same
        // trim_end as the VM.
        let i64_ty = self.context.i64_type();
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let read_line_fn = self.get_runtime_fn("mimi_read_stdin_line")?;
        let raw = self
            .build_call(read_line_fn, &[], "read_stdin_line_call")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_read_stdin_line returned void".into()))?
            .into_pointer_value();

        // Build string struct { i8*, i64 } (result slot shared by both arms).
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let str_alloca = self.build_alloca(string_ty, "input_str")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;

        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("no current function for input".into()))?;
        let eof_bb = self.context.append_basic_block(function, "input_eof");
        let ok_bb = self.context.append_basic_block(function, "input_ok");
        let merge_bb = self.context.append_basic_block(function, "input_merge");
        let raw_null = self
            .builder
            .build_is_null(raw, "input_raw_null")
            .map_err(|e| CompileError::LlvmError(format!("is_null error: {}", e)))?;
        self.build_cond_br(raw_null, eof_bb, ok_bb)?;

        // EOF/error arm: deterministic empty string.
        self.builder.position_at_end(eof_bb);
        let empty_lit = self
            .builder
            .build_global_string_ptr("", "input_empty")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_store(ptr_gep, empty_lit.as_pointer_value())?;
        self.build_store(len_gep, i64_ty.const_int(0, false))?;
        self.build_br(merge_bb)?;

        // Success arm: runtime helper already trimmed trailing whitespace and
        // NUL-terminated the heap buffer.
        self.builder.position_at_end(ok_bb);
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let str_len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(raw)],
                "strlen_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        self.build_store(ptr_gep, raw)?;
        self.build_store(len_gep, str_len)?;
        self.build_br(merge_bb)?;

        self.builder.position_at_end(merge_bb);
        // Central fix 2026-08-05: return the LOADED {ptr, i64} struct value.
        // Returning the alloca pointer made the resolved emitter's
        // wrap_builtin_string_result treat it as a raw char* and double-wrap
        // it ({ptr: &struct, len: 0}) → garbage len/data.
        let loaded = self.build_load(
            BasicTypeEnum::StructType(string_ty),
            str_alloca,
            "input_result",
        )?;
        Ok(loaded)
    }

    pub(super) fn compile_try_input_line(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if !args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "try_input_line expects 0 arguments".to_string(),
            ));
        }

        // Mirrors compile_getenv's Result<string,string> lowering. The
        // runtime helper returns null on EOF/read error and a heap string on
        // a successful line (including empty lines), so the two cases are
        // distinguishable.
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let bool_ty = self.context.bool_type();
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        // Result<string,string> layout: {i1 disc, string ok, i64 err}
        let result_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::StructType(string_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        let read_line_fn = self.get_runtime_fn("mimi_read_stdin_line")?;
        let raw = self
            .build_call(read_line_fn, &[], "try_input_line_call")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_read_stdin_line returned void".into()))?
            .into_pointer_value();

        let str_alloca = self.build_alloca(string_ty, "try_input_str")?;
        let result_alloca = self.build_alloca(result_ty, "try_input_result")?;

        let str_ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let str_len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(str_ptr_gep, i8_ptr.const_null())?;
        self.build_store(str_len_gep, i64_ty.const_int(0, false))?;

        let disc_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 0, "res_disc")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let ok_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 1, "res_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let err_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 2, "res_err")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;

        let is_null = self
            .builder
            .build_is_null(raw, "try_input_is_null")
            .map_err(|e| CompileError::LlvmError(format!("is_null error: {}", e)))?;
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("no current function for try_input_line".into())
        })?;
        let ok_bb = self.context.append_basic_block(function, "try_input_ok");
        let err_bb = self.context.append_basic_block(function, "try_input_err");
        let merge_bb = self.context.append_basic_block(function, "try_input_merge");
        self.build_cond_br(is_null, err_bb, ok_bb)?;

        // Ok branch: disc=1, ok=string, err=0
        self.builder.position_at_end(ok_bb);
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let str_len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(raw)],
                "try_input_strlen",
            )?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        self.build_store(str_ptr_gep, raw)?;
        self.build_store(str_len_gep, str_len)?;
        self.build_store(disc_gep, bool_ty.const_int(1, false))?;
        let str_val = self.build_load(string_ty, str_alloca, "try_input_str_val")?;
        self.build_store(ok_gep, str_val)?;
        self.build_store(err_gep, i64_ty.const_int(0, false))?;
        self.build_br(merge_bb)?;

        // Err branch: disc=0, ok=zero, err=heap {ptr,len} string handle
        self.builder.position_at_end(err_bb);
        let err_msg = self
            .builder
            .build_global_string_ptr("input: EOF or read error", "try_input_err_msg")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let err_len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(
                    err_msg.as_pointer_value(),
                )],
                "try_input_err_len",
            )?
            .try_as_basic_value_opt()
            .ok_or("try_input strlen returned void")?
            .into_int_value();
        let heap = self.malloc_or_abort(i64_ty.const_int(16, false), "try_input_err_heap")?;
        let heap_ptr = self
            .build_bit_cast(
                heap.into(),
                BasicTypeEnum::PointerType(i8_ptr),
                "try_input_err_heap_ptr",
            )?
            .into_pointer_value();
        let err_gep0 = self
            .gep()
            .build_struct_gep(string_ty, heap_ptr, 0, "try_input_err_heap_ptr_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(err_gep0, err_msg.as_pointer_value())?;
        let err_gep1 = self
            .gep()
            .build_struct_gep(string_ty, heap_ptr, 1, "try_input_err_heap_len_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(err_gep1, err_len)?;
        self.build_store(disc_gep, bool_ty.const_int(0, false))?;
        self.build_store(ok_gep, string_ty.const_zero())?;
        let err_ptr_int = self.build_ptr_to_int(heap_ptr, i64_ty, "try_input_err_ptr_int")?;
        self.build_store(err_gep, err_ptr_int)?;
        self.build_br(merge_bb)?;

        self.builder.position_at_end(merge_bb);
        self.build_load(result_ty, result_alloca, "try_input_result_loaded")
    }

    pub(super) fn compile_file_exists(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "file_exists expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        // access(path, F_OK) where F_OK = 0
        let i32_ty = self.context.i32_type();
        let access_fn = self.module.get_function("access").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i32_ty),
                ],
                false,
            );
            self.module
                .add_function("access", ty, Some(inkwell::module::Linkage::External))
        });
        let ret = self
            .builder
            .build_call(
                access_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(0, false)),
                ],
                "access_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("access error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("access returned void")?;
        let zero = i32_ty.const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                ret.into_int_value(),
                zero,
                "exists",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        // 2026-08-06 (audit 1e): return i1 (bool) — the checker infers `bool`
        // for file_exists; zext to i64 made native print "1" vs VM "true".
        Ok(cmp.into())
    }

    pub(super) fn compile_read_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "read_file expects 1 argument".to_string(),
            ));
        }
        self.compile_read_file_inner(&args[0])
    }

    /// Phase D (0.39.75): 收 cap 的 fs API——SystemToken 能力门禁在 args[1]
    /// （运行时忽略：线性消费由 checker/CFG 保证），path 在 args[0]。复用
    /// read_file 核心（Result<string,string> 布局一致）。
    pub(super) fn compile_read_file_guarded(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "read_file_guarded expects 2 arguments (path, a SystemToken capability)"
                    .to_string(),
            ));
        }
        self.compile_read_file_inner(&args[0])
    }

    fn compile_read_file_inner(
        &self,
        path_arg: &BasicMetadataValueEnum<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let path_ptr = self.extract_raw_str_ptr(path_arg)?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let bool_ty = self.context.bool_type();
        let i32_ty = self.context.i32_type();
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        // Result<string, string> layout: {i1 disc, string ok, i64 err}
        let result_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::StructType(string_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        let str_alloca = self
            .builder
            .build_alloca(string_ty, "read_str")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let result_alloca = self
            .builder
            .build_alloca(result_ty, "read_result")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;

        // Compute GEPs for string struct fields (used in Ok branch only)
        let str_ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let str_len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        // Compute GEPs for Result struct fields (used in both branches)
        let disc_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 0, "res_disc")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let ok_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 1, "res_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let err_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 2, "res_err")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;

        // fopen(path, "r")
        let mode_str = self
            .builder
            .build_global_string_ptr("r", "read_mode")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let fopen_fn = self.module.get_function("fopen").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i8_ptr.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fopen", ty, Some(inkwell::module::Linkage::External))
        });
        let file = self
            .builder
            .build_call(
                fopen_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ],
                "fopen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fopen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("fopen returned void")?
            .into_pointer_value();

        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function".to_string())?;
        let fopen_ok_bb = self.context.append_basic_block(function, "fopen_ok");
        let fopen_null_bb = self.context.append_basic_block(function, "fopen_null");
        let merge_bb = self.context.append_basic_block(function, "read_merge");

        let is_null = self
            .builder
            .build_int_compare(IntPredicate::EQ, file, i8_ptr_ty.const_zero(), "fopen_null")
            .map_err(|e| CompileError::LlvmError(format!("null compare error: {}", e)))?;
        self.build_cond_br(is_null, fopen_null_bb, fopen_ok_bb)?;

        // ── Ok branch: fopen succeeded ──
        self.builder.position_at_end(fopen_ok_bb);

        // fseek(file, 0, SEEK_END)
        let fseek_fn = self.module.get_function("fseek").unwrap_or_else(|| {
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::IntType(i32_ty),
                ],
                false,
            );
            self.module
                .add_function("fseek", ty, Some(inkwell::module::Linkage::External))
        });
        let fseek_result = self
            .builder
            .build_call(
                fseek_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(file),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(0, false)),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(2, false)), // SEEK_END
                ],
                "fseek_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fseek error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("fseek returned void")?
            .into_int_value();
        let fseek_ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                fseek_result,
                i32_ty.const_int(0, false),
                "fseek_ok",
            )
            .map_err(|e| CompileError::LlvmError(format!("fseek compare: {}", e)))?;
        // ftell(file) -> file size
        let ftell_fn = self.module.get_function("ftell").unwrap_or_else(|| {
            let ty = i64_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
            self.module
                .add_function("ftell", ty, Some(inkwell::module::Linkage::External))
        });
        let file_size = self
            .builder
            .build_call(
                ftell_fn,
                &[BasicMetadataValueEnum::PointerValue(file)],
                "ftell_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("ftell error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("ftell returned void")?
            .into_int_value();
        // Clamp negative file_size to 0
        let zero = i64_ty.const_int(0, false);
        let neg_one = i64_ty.const_int(u64::MAX, false);
        let is_neg_one = self
            .builder
            .build_int_compare(IntPredicate::EQ, file_size, neg_one, "is_neg_one")
            .map_err(|e| CompileError::LlvmError(format!("neg_one compare: {}", e)))?;
        let fseek_failed = self
            .builder
            .build_xor(fseek_ok, bool_ty.const_int(1, false), "fseek_failed")
            .map_err(|e| CompileError::LlvmError(format!("xor error: {}", e)))?;
        let clamp_cond = self
            .builder
            .build_or(fseek_failed, is_neg_one, "clamp_cond")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let file_size = self
            .builder
            .build_select(clamp_cond, zero, file_size, "file_size")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        // rewind(file)
        let rewind_fn = self.module.get_function("rewind").unwrap_or_else(|| {
            let ty = self
                .context
                .void_type()
                .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
            self.module
                .add_function("rewind", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(
            rewind_fn,
            &[BasicMetadataValueEnum::PointerValue(file)],
            "rewind_call",
        )?;
        // Guard against i64::MAX + 1 wrapping (batch4-01 P2-5). Real files
        // never reach this size, but a hostile/truncated ftell must not turn
        // the +1 into a negative malloc size.
        let is_max_size = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                file_size,
                i64_ty.const_int(i64::MAX as u64, false),
                "read_file_size_is_max",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("read_file: no current function".into()))?;
        let size_ok_bb = self.context.append_basic_block(function, "read_size_ok_bb");
        let size_trap_bb = self
            .context
            .append_basic_block(function, "read_size_trap_bb");
        self.builder
            .build_conditional_branch(is_max_size, size_trap_bb, size_ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("cbr error: {}", e)))?;
        self.builder.position_at_end(size_trap_bb);
        let abort_fn = self.get_or_declare_abort_fn();
        let size_msg = self
            .builder
            .build_global_string_ptr("read_file: file size too large", "read_size_msg")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(
                size_msg.as_pointer_value(),
            )],
            "read_size_abort",
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        self.builder.position_at_end(size_ok_bb);
        // malloc(file_size + 1)
        let one = i64_ty.const_int(1, false);
        let alloc_size = self
            .builder
            .build_int_add(file_size, one, "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        // B4: use malloc_or_abort for NULL check.
        let buf = self.malloc_or_abort(alloc_size, "read_buf")?;
        // fread(buf, 1, file_size, file)
        let fread_fn = self.module.get_function("fread").unwrap_or_else(|| {
            let ty = i64_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                ],
                false,
            );
            self.module
                .add_function("fread", ty, Some(inkwell::module::Linkage::External))
        });
        let fread_ret = self
            .build_call(
                fread_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(1, false)),
                    BasicMetadataValueEnum::IntValue(file_size),
                    BasicMetadataValueEnum::PointerValue(file),
                ],
                "fread_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("fread returned void")?
            .into_int_value();
        // The file may have been truncated or read short between stat and
        // fread. Use the actual byte count for both the terminator and the
        // returned string length, never uninitialized tail bytes (batch4-01
        // P2-4). fread cannot legally return more than the requested count.
        let read_len = fread_ret;
        // Null-terminate
        let null_gep = self
            .build_in_bounds_gep(
                BasicTypeEnum::IntType(self.context.i8_type()),
                buf,
                &[read_len],
                "null_byte",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(null_gep, self.context.i8_type().const_int(0, false))?;
        // fclose(file)
        let fclose_fn = self.module.get_function("fclose").unwrap_or_else(|| {
            let ty = i32_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
            self.module
                .add_function("fclose", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(
            fclose_fn,
            &[BasicMetadataValueEnum::PointerValue(file)],
            "fclose_call",
        )?;

        // Build string struct {i8*, i64} and store into Ok
        self.build_store(str_ptr_gep, buf)?;
        self.build_store(str_len_gep, read_len)?;

        self.build_store(disc_gep, bool_ty.const_int(1, false))?;
        let str_val = self.build_load(string_ty, str_alloca, "str_val")?;
        self.build_store(ok_gep, str_val)?;
        self.build_store(err_gep, i64_ty.const_int(0, false))?;
        self.build_br(merge_bb)?;

        // ── Err branch: fopen returned NULL ──
        self.builder.position_at_end(fopen_null_bb);
        // Deep-eval 2026-08-09 (test_result_match parity): format the OS error
        // like the interpreter's `e.to_string()` ("No such file or directory
        // (os error 2)") instead of a hard-coded message.
        let os_err_fn = self
            .module
            .get_function("mimi_os_error_message")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_os_error_message",
                    i8_ptr_ty.fn_type(&[], false),
                    Some(inkwell::module::Linkage::External),
                )
            });
        let err_msg = self
            .builder
            .build_call(os_err_fn, &[], "read_err_msg")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .try_as_basic_value_opt()
            .ok_or("mimi_os_error_message void")?
            .into_pointer_value();
        // NOTE: Ownership intentionally stays with the returned Result value.
        // Registering these pointers in the function-level heap scope would free
        // them on an early return/function boundary, causing use-after-free when
        // the caller decodes the Err string. Leaks on local-only errors are
        // tracked separately (audit P2); correctness/UAF must win.
        self.build_store(disc_gep, bool_ty.const_int(0, false))?;
        self.build_store(ok_gep, string_ty.const_zero())?;
        // Err(string) must store a heap {ptr,len} handle — the contract that
        // match decode (inttoptr+GEP+load), `?` and display's struct probe
        // expect. A bare data pointer makes match decode load string bytes as
        // a {ptr,len} struct (garbage field0) and concat strlen-segfaults.
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let heap = self.malloc_or_abort(i64_ty.const_int(16, false), "read_err_heap")?;
        let heap_ptr = self
            .build_bit_cast(
                heap.into(),
                BasicTypeEnum::PointerType(i8_ptr_ty),
                "read_err_heap_ptr",
            )?
            .into_pointer_value();
        let err_gep0 = self
            .gep()
            .build_struct_gep(string_ty, heap_ptr, 0, "read_err_heap_ptr_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(err_gep0, err_msg)?;
        let err_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(err_msg)],
                "read_err_len",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .try_as_basic_value_opt()
            .ok_or("read_file strlen void")?
            .into_int_value();
        let err_gep1 = self
            .gep()
            .build_struct_gep(string_ty, heap_ptr, 1, "read_err_heap_len_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(err_gep1, err_len)?;
        let err_ptr_int = self.build_ptr_to_int(heap_ptr, i64_ty, "err_ptr_int")?;
        self.build_store(err_gep, err_ptr_int)?;
        self.build_br(merge_bb)?;

        // ── Merge ──
        self.builder.position_at_end(merge_bb);
        self.build_load(result_ty, result_alloca, "read_file_loaded")
    }

    pub(super) fn compile_write_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "write_file expects 2 arguments".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let (content_ptr, content_len) = self.extract_raw_str_ptr_len(&args[1])?;
        // fopen(path, "w")
        let mode_str = self
            .builder
            .build_global_string_ptr("w", "write_mode")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let fopen_fn = self.module.get_function("fopen").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i8_ptr.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fopen", ty, Some(inkwell::module::Linkage::External))
        });
        let file = self
            .builder
            .build_call(
                fopen_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ],
                "fopen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fopen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("fopen returned void")?
            .into_pointer_value();
        // Result<(), string> layout: {i1 disc, i64 ok, i64 err}
        let bool_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let ok_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function".to_string())?;
        let null_check_bb = self
            .context
            .append_basic_block(function, "fopen_null_check");
        let write_bb = self.context.append_basic_block(function, "fopen_not_null");
        let merge_bb = self.context.append_basic_block(function, "write_merge");
        let alloca = self.build_alloca(BasicTypeEnum::StructType(ok_ty), "write_result")?;
        let disc_gep = self
            .gep()
            .build_struct_gep(ok_ty, alloca, 0, "wr_disc")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        let ok_gep = self
            .gep()
            .build_struct_gep(ok_ty, alloca, 1, "wr_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        let err_gep = self
            .gep()
            .build_struct_gep(ok_ty, alloca, 2, "wr_err")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        let is_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                file,
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .const_null(),
                "fopen_is_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(is_null, null_check_bb, write_bb)
            .map_err(|e| CompileError::LlvmError(format!("cbr error: {}", e)))?;
        // ── Err branch: fopen returned NULL ──
        // Result Err: {i1 false, i64 0, i64 err_msg_handle}
        self.builder.position_at_end(null_check_bb);
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        // Deep-eval 2026-08-09 (test_result_match parity): OS error message
        // matching the interpreter (see compile_read_file).
        let os_err_fn = self
            .module
            .get_function("mimi_os_error_message")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_os_error_message",
                    i8_ptr_ty.fn_type(&[], false),
                    Some(inkwell::module::Linkage::External),
                )
            });
        let err_msg = self
            .builder
            .build_call(os_err_fn, &[], "write_err_msg")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .try_as_basic_value_opt()
            .ok_or("mimi_os_error_message void")?
            .into_pointer_value();
        // Do not register in the function-level heap scope: like compile_read_file,
        // registering would free the Err string on an early return/function boundary
        // while the returned Result still references it.
        self.build_store(disc_gep, bool_ty.const_int(0, false))?;
        self.build_store(ok_gep, i64_ty.const_int(0, false))?;
        // Err(string) must use a heap {ptr,len} handle (see compile_read_file)
        // — a bare data pointer segfaults match decode / `?` (inttoptr+load
        // of bytes).
        let string_struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let heap = self.malloc_or_abort(i64_ty.const_int(16, false), "write_err_heap")?;
        let heap_ptr = self
            .build_bit_cast(
                heap.into(),
                BasicTypeEnum::PointerType(i8_ptr_ty),
                "write_err_heap_ptr",
            )?
            .into_pointer_value();
        let werr_gep0 = self
            .gep()
            .build_struct_gep(string_struct_ty, heap_ptr, 0, "write_err_heap_ptr_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(werr_gep0, err_msg)?;
        let werr_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(err_msg)],
                "write_err_len",
            )
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .try_as_basic_value_opt()
            .ok_or("write_file strlen void")?
            .into_int_value();
        let werr_gep1 = self
            .gep()
            .build_struct_gep(string_struct_ty, heap_ptr, 1, "write_err_heap_len_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(werr_gep1, werr_len)?;
        let err_ptr_int = self.build_ptr_to_int(heap_ptr, i64_ty, "err_ptr_int")?;
        self.build_store(err_gep, err_ptr_int)?;
        self.build_br(merge_bb)?;
        // ── Ok branch: fopen succeeded ──
        self.builder.position_at_end(write_bb);
        // Use the Mimi string's explicit byte length so embedded NUL bytes
        // are written intact (batch4-01 P2-6).
        // fwrite(content, 1, len, file)
        let fwrite_fn = self.module.get_function("fwrite").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = self.context.i64_type().fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fwrite", ty, Some(inkwell::module::Linkage::External))
        });
        let fwrite_result = self.build_call(
            fwrite_fn,
            &[
                BasicMetadataValueEnum::PointerValue(content_ptr),
                BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(1, false)),
                BasicMetadataValueEnum::IntValue(content_len),
                BasicMetadataValueEnum::PointerValue(file),
            ],
            "fwrite_call",
        )?;
        let fwrite_int = fwrite_result
            .try_as_basic_value_opt()
            .ok_or("fwrite returned void")?
            .into_int_value();
        // fclose(file)
        let i32_ty = self.context.i32_type();
        let fclose_fn = self.module.get_function("fclose").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i32_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
            self.module
                .add_function("fclose", ty, Some(inkwell::module::Linkage::External))
        });
        let fclose_result = self.build_call(
            fclose_fn,
            &[BasicMetadataValueEnum::PointerValue(file)],
            "fclose_call",
        )?;
        let fclose_int = fclose_result
            .try_as_basic_value_opt()
            .ok_or("fclose returned void")?
            .into_int_value();
        // A short fwrite or a failed fclose must be reported as Err, not
        // silently reported as a successful write.
        let wrote_short = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                fwrite_int,
                content_len,
                "write_wrote_short",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let close_failed = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                fclose_int,
                i32_ty.const_int(0, false),
                "write_close_failed",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let write_failed = self
            .builder
            .build_or(wrote_short, close_failed, "write_failed")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let write_success_bb = self
            .context
            .append_basic_block(function, "write_success_bb");
        self.builder
            .build_conditional_branch(write_failed, null_check_bb, write_success_bb)
            .map_err(|e| CompileError::LlvmError(format!("cbr error: {}", e)))?;
        self.builder.position_at_end(write_success_bb);
        // Result Ok: {i1 true, i64 0, i64 0}
        self.build_store(disc_gep, bool_ty.const_int(1, false))?;
        self.build_store(ok_gep, i64_ty.const_int(0, false))?;
        self.build_store(err_gep, i64_ty.const_int(0, false))?;
        self.build_br(merge_bb)?;
        // ── Merge ──
        self.builder.position_at_end(merge_bb);
        self.build_load(
            BasicTypeEnum::StructType(ok_ty),
            alloca,
            "write_file_loaded",
        )
    }

    /// Print a single value to stdout for assert_eq diagnostics.
    fn build_print_value(
        &self,
        printf: FunctionValue<'ctx>,
        val: &BasicMetadataValueEnum<'ctx>,
    ) -> Result<(), CompileError> {
        match val {
            BasicMetadataValueEnum::IntValue(iv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%lld", "int_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                self.build_call(
                    printf,
                    &[
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::IntValue(*iv),
                    ],
                    "print_int",
                )
                .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%f", "float_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                self.build_call(
                    printf,
                    &[
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::FloatValue(*fv),
                    ],
                    "print_float",
                )
                .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
            }
            BasicMetadataValueEnum::PointerValue(pv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%s", "str_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                self.build_call(
                    printf,
                    &[
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(*pv),
                    ],
                    "print_str",
                )
                .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                if let Ok(BasicValueEnum::PointerValue(pv)) =
                    self.build_extract_value((*sv).into(), 0, "str_field")
                {
                    let fmt = self
                        .builder
                        .build_global_string_ptr("%s", "struct_str_fmt")
                        .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                    self.build_call(
                        printf,
                        &[
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(pv),
                        ],
                        "print_struct_str",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // === Directory & path operations (codegen) ===

    fn call_runtime_str_to_bool(
        &self,
        runtime_fn_name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(format!(
                "{} expects 1 argument",
                runtime_fn_name
            )));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self
            .module
            .get_function(runtime_fn_name)
            .ok_or_else(|| CompileError::LlvmError(format!("{} not declared", runtime_fn_name)))?;
        let result = self
            .build_call(
                fn_val,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                &format!("{}_call", runtime_fn_name),
            )
            .map_err(|e| CompileError::LlvmError(format!("{}: {}", runtime_fn_name, e)))?;
        let ret = result
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError(format!("{} returned void", runtime_fn_name)))?;
        // 2026-08-06 (audit 1e): the runtime predicate returns a C int (0/1);
        // the checker infers `bool` for these builtins — normalize to i1 so
        // native prints "true"/"false" like the VM (was "1"/"0", L1 divergence).
        let ret_int = ret.into_int_value();
        let zero = ret_int.get_type().const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, ret_int, zero, "to_bool")
            .map_err(|e| CompileError::LlvmError(format!("{} to_bool: {}", runtime_fn_name, e)))?;
        Ok(cmp.into())
    }

    fn call_runtime_str_to_str(
        &self,
        runtime_fn_name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(format!(
                "{} expects 1 argument",
                runtime_fn_name
            )));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self
            .module
            .get_function(runtime_fn_name)
            .ok_or_else(|| CompileError::LlvmError(format!("{} not declared", runtime_fn_name)))?;
        let result = self
            .build_call(
                fn_val,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                &format!("{}_call", runtime_fn_name),
            )
            .map_err(|e| CompileError::LlvmError(format!("{}: {}", runtime_fn_name, e)))?;
        let raw_ptr = result
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError(format!("{} returned void", runtime_fn_name)))?;
        // Wrap raw C string into Mimi string struct {ptr, len}
        self.wrap_c_string(raw_ptr.into_pointer_value())
    }

    pub(super) fn compile_listdir(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "listdir expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self
            .module
            .get_function("mimi_listdir")
            .ok_or("mimi_listdir not declared")?;
        let result = self
            .build_call(
                fn_val,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                "listdir_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("listdir: {}", e)))?;
        let list_ptr = result
            .try_as_basic_value_opt()
            .ok_or("listdir returned void")?;
        // Return as opaque pointer (MimiList*)
        Ok(list_ptr)
    }

    pub(super) fn compile_is_dir(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_is_dir", args)
    }

    pub(super) fn compile_is_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_is_file", args)
    }

    pub(super) fn compile_path_join(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "path_join expects 2 arguments".to_string(),
            ));
        }
        let a_ptr = self.extract_raw_str_ptr(&args[0])?;
        let b_ptr = self.extract_raw_str_ptr(&args[1])?;
        let fn_val = self
            .module
            .get_function("mimi_path_join")
            .ok_or("mimi_path_join not declared")?;
        let result = self
            .build_call(
                fn_val,
                &[
                    BasicMetadataValueEnum::PointerValue(a_ptr),
                    BasicMetadataValueEnum::PointerValue(b_ptr),
                ],
                "path_join_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("path_join: {}", e)))?;
        let raw_ptr = result
            .try_as_basic_value_opt()
            .ok_or("path_join returned void")?;
        self.wrap_c_string(raw_ptr.into_pointer_value())
    }

    pub(super) fn compile_path_ext(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_path_ext", args)
    }

    pub(super) fn compile_path_basename(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_path_basename", args)
    }

    pub(super) fn compile_path_dirname(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_path_dirname", args)
    }

    pub(super) fn compile_walk_dir(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "walk_dir expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self
            .module
            .get_function("mimi_walk_dir")
            .ok_or("mimi_walk_dir not declared")?;
        let result = self
            .build_call(
                fn_val,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                "walk_dir_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("walk_dir: {}", e)))?;
        let list_ptr = result
            .try_as_basic_value_opt()
            .ok_or("walk_dir returned void")?;
        Ok(list_ptr)
    }

    pub(super) fn compile_mkdir_p(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_mkdir_p", args)
    }

    pub(super) fn compile_remove_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_remove_file", args)
    }

    // === Process & advanced file operations (codegen) ===

    pub(super) fn compile_exec(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "exec expects 1 argument".to_string(),
            ));
        }
        let cmd_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());

        // Call mimi_exec(cmd) -> MimiExecResult*
        let exec_fn = self.get_runtime_fn("mimi_exec")?;
        let res_ptr = self
            .build_call(
                exec_fn,
                &[BasicMetadataValueEnum::PointerValue(cmd_ptr)],
                "exec_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("exec error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_exec returned void")?
            .into_pointer_value();

        // MimiExecResult layout: { i64 exit_code, i8* stdout, i8* stderr }
        let res_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        );

        // Extract exit_code
        let exit_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 0, "exit_code_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let exit_code_raw = self
            .build_load(self.context.i64_type(), exit_gep, "exit_code_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // Truncate to i32 for ExecResult.exit_code field
        let exit_code = self
            .builder
            .build_int_truncate(exit_code_raw, self.context.i32_type(), "exit_code_i32")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;

        // Extract stdout
        let stdout_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 1, "stdout_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stdout_raw = self
            .build_load(i8_ptr, stdout_gep, "stdout_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stdout_str = self.wrap_c_string(stdout_raw)?;

        // Extract stderr
        let stderr_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 2, "stderr_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stderr_raw = self
            .build_load(i8_ptr, stderr_gep, "stderr_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stderr_str = self.wrap_c_string(stderr_raw)?;

        // Free the runtime struct (not the strings — they're owned by ExecResult)
        let free_struct_fn = self.get_runtime_fn("mimi_exec_free_struct")?;
        self.build_call(
            free_struct_fn,
            &[BasicMetadataValueEnum::PointerValue(res_ptr)],
            "exec_free_struct",
        )?;

        // Build ExecResult LLVM struct { i32, {i8*,i64}, {i8*,i64} }
        let string_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let exec_result_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i32_type()),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(exec_result_ty, "exec_result")?;

        // Store exit_code
        let f0 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 0, "f0")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(f0, exit_code)?;

        // Store stdout string
        let f1 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 1, "f1")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(f1, stdout_str)?;

        // Store stderr string
        let f2 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 2, "f2")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(f2, stderr_str)?;

        Ok(alloca.into())
    }

    pub(super) fn compile_file_stat(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "file_stat expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // Allocate err_out pointer
        let err_alloca = self.build_alloca(i8_ptr, "err_out")?;
        self.build_store(err_alloca, i8_ptr.const_null())?;

        // Call mimi_file_stat(path, &err_out)
        let stat_fn = self.get_runtime_fn("mimi_file_stat")?;
        let stat_ptr = self
            .build_call(
                stat_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(err_alloca),
                ],
                "stat_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("file_stat error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_file_stat returned void")?
            .into_pointer_value();

        // MimiStatResult layout: { i64 size, i64 modified, i64 is_file, i64 is_dir }
        let mimi_stat_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        // Check if stat_ptr is null (error case)
        let is_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                stat_ptr,
                i8_ptr.const_null(),
                "stat_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;

        // Build StatResult LLVM struct { i64, i64, i1, i1 }
        let bool_ty = self.context.bool_type();
        let stat_result_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(bool_ty),
                inkwell::types::BasicTypeEnum::IntType(bool_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(stat_result_ty, "stat_result")?;

        // MEM-C7 (deep audit): use conditional branch instead of select.
        // LLVM evaluates both sides of a select, so GEP+load on NULL would execute
        // even when is_null is true, causing UB. Branch to avoid the GEP entirely.
        let zero_i64 = i64_ty.const_int(0, false);
        let neg_one_i64 = i64_ty.const_int((-1i64) as u64, false);
        let false_val = bool_ty.const_int(0, false);

        let function = self.current_function().ok_or(CompileError::LlvmError(
            "no current function for file_stat".into(),
        ))?;
        let null_bb = self.context.append_basic_block(function, "stat_null_bb");
        let nonnull_bb = self.context.append_basic_block(function, "stat_nonnull_bb");
        let merge_bb = self.context.append_basic_block(function, "stat_merge_bb");

        self.builder
            .build_conditional_branch(is_null, null_bb, nonnull_bb)
            .map_err(|e| CompileError::LlvmError(format!("cbr error: {}", e)))?;

        // Null path: use default values. Release the runtime error string if
        // one was written into err_out (batch4-01 P2-3).
        self.builder.position_at_end(null_bb);
        let null_size = neg_one_i64;
        let null_mod = zero_i64;
        let null_isf = false_val;
        let null_isd = false_val;
        let err_out = self
            .build_load(i8_ptr, err_alloca, "stat_err_out")
            .map_err(|e| CompileError::LlvmError(format!("load err_out: {}", e)))?
            .into_pointer_value();
        let free_fn = self.get_runtime_fn("free")?;
        self.build_call(
            free_fn,
            &[BasicMetadataValueEnum::PointerValue(err_out)],
            "stat_free_err_out",
        )?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;

        // Non-null path: load fields from stat_ptr
        self.builder.position_at_end(nonnull_bb);
        // size
        let size_gep = self
            .gep()
            .build_struct_gep(mimi_stat_ty, stat_ptr, 0, "size_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let nn_size = self
            .build_load(i64_ty, size_gep, "size_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // modified
        let mod_gep = self
            .gep()
            .build_struct_gep(mimi_stat_ty, stat_ptr, 1, "mod_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let nn_mod = self
            .build_load(i64_ty, mod_gep, "mod_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // is_file
        let isf_gep = self
            .gep()
            .build_struct_gep(mimi_stat_ty, stat_ptr, 2, "isf_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let isf_raw = self
            .build_load(i64_ty, isf_gep, "isf_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let nn_isf = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, isf_raw, zero_i64, "isf_cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        // is_dir
        let isd_gep = self
            .gep()
            .build_struct_gep(mimi_stat_ty, stat_ptr, 3, "isd_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let isd_raw = self
            .build_load(i64_ty, isd_gep, "isd_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let nn_isd = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, isd_raw, zero_i64, "isd_cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;

        // Merge: phi nodes for each field
        self.builder.position_at_end(merge_bb);
        let size_phi = self
            .builder
            .build_phi(i64_ty, "size_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        size_phi.add_incoming(&[(&null_size, null_bb), (&nn_size, nonnull_bb)]);
        let size_val: BasicValueEnum = size_phi.as_basic_value();
        let mod_phi = self
            .builder
            .build_phi(i64_ty, "mod_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        mod_phi.add_incoming(&[(&null_mod, null_bb), (&nn_mod, nonnull_bb)]);
        let mod_val: BasicValueEnum = mod_phi.as_basic_value();
        let isf_phi = self
            .builder
            .build_phi(bool_ty, "isf_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        isf_phi.add_incoming(&[(&null_isf, null_bb), (&nn_isf, nonnull_bb)]);
        let isf_val: BasicValueEnum = isf_phi.as_basic_value();
        let isd_phi = self
            .builder
            .build_phi(bool_ty, "isd_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        isd_phi.add_incoming(&[(&null_isd, null_bb), (&nn_isd, nonnull_bb)]);
        let isd_val: BasicValueEnum = isd_phi.as_basic_value();

        // Store into StatResult struct
        let s0 = self
            .gep()
            .build_struct_gep(stat_result_ty, alloca, 0, "s0")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(s0, size_val)?;
        let s1 = self
            .gep()
            .build_struct_gep(stat_result_ty, alloca, 1, "s1")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(s1, mod_val)?;
        let s2 = self
            .gep()
            .build_struct_gep(stat_result_ty, alloca, 2, "s2")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(s2, isf_val)?;
        let s3 = self
            .gep()
            .build_struct_gep(stat_result_ty, alloca, 3, "s3")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(s3, isd_val)?;

        // Free the stat result (uses Rust allocator via Box::from_raw)
        let free_fn = self.get_runtime_fn("mimi_file_stat_free")?;
        self.build_call(
            free_fn,
            &[BasicMetadataValueEnum::PointerValue(stat_ptr)],
            "stat_free",
        )?;

        Ok(alloca.into())
    }

    pub(super) fn compile_append_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "append_file expects 2 arguments".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let (content_ptr, content_len) = self.extract_raw_str_ptr_len(&args[1])?;

        let append_fn = self.get_runtime_fn("mimi_append_file_ll")?;
        let ret = self
            .build_call(
                append_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(content_ptr),
                    BasicMetadataValueEnum::IntValue(content_len),
                ],
                "append_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("append_file error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_append_file_ll returned void")?
            .into_int_value();

        // Convert i64 to bool (i64): ret != 0
        let zero = self.context.i64_type().const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, ret, zero, "append_ok")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let result = self
            .builder
            .build_int_z_extend(cmp, self.context.i64_type(), "append_result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(result.into())
    }

    pub(super) fn compile_set_env(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "set_env expects 2 arguments".to_string(),
            ));
        }
        let key_ptr = self.extract_raw_str_ptr(&args[0])?;
        let val_ptr = self.extract_raw_str_ptr(&args[1])?;

        let set_fn = self.get_runtime_fn("mimi_set_env")?;
        let ret = self
            .build_call(
                set_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                    BasicMetadataValueEnum::PointerValue(val_ptr),
                ],
                "set_env_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("set_env error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_set_env returned void")?
            .into_int_value();

        // Convert i64 to bool (i64): ret != 0
        let zero = self.context.i64_type().const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, ret, zero, "set_env_ok")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let result = self
            .builder
            .build_int_z_extend(cmp, self.context.i64_type(), "set_env_result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(result.into())
    }

    // === Crypto operations (codegen) ===

    pub(super) fn compile_sha256(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_sha256", args)
    }

    pub(super) fn compile_base64_encode(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_base64_encode", args)
    }

    pub(super) fn compile_base64_decode(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_base64_decode", args)
    }

    pub(super) fn compile_format(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "format expects at least 1 argument (template string)".to_string(),
            ));
        }
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // Template string (second runtime arg).
        // Unwrap StructValue {i8*, i64} to PointerValue(i8*) if needed
        let template_val = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "template_ptr")
                .map_err(|e| {
                    CompileError::LlvmError(format!("extract format template ptr: {}", e))
                })?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "format: first arg must be a string template".to_string(),
                ))
            }
        };
        // Wave-1 audit fix (§8, FIX: format() hard-capped substitutions at 8
        // while reporting the true count): convert ALL substitution args to
        // C string pointers — no `.min(9)` cap. `mimi_str_format`'s ABI
        // still takes 8 arg slots per call, so >8 args are applied by
        // chaining calls: each call substitutes the next ≤8 `{}`
        // placeholders left-to-right, preserving overall substitution order
        // (the runtime always fills placeholders sequentially from the
        // current string). Known edge vs the single-pass VM: when >8 args
        // are used AND an earlier substituted value itself contains "{}",
        // later passes may substitute inside that value.
        // H-17 fix: type info is staged by the call dispatcher
        // (`pending_print_arg_types`, same as print/println) — read it and
        // lower every supported arg type through `format_arg_to_cstr`
        // (print-family emitters). Previously any non-string StructValue
        // (e.g. a List's `{i64 len, ptr data}`) reached
        // `.into_pointer_value()` on the length field and panicked the
        // compiler (ICE); unknown shapes got a silent null pointer.
        let arg_types: Vec<String> = self.pending_print_arg_types.clone();
        // H-17: snapshot the display-buffer registry *before* converting
        // substitution args. Aggregate emitters (List/Map/...) register their
        // heap strings via `register_display_alloc`; unlike print/println,
        // format consumes them inside `mimi_str_format` and must release them
        // here — otherwise the pointers linger in `display_frees` and the next
        // function's `flush_display_frees` emits a `free` on a foreign
        // function's SSA value (invalid IR → LLVM crash).
        let disp_marker = self.display_marker();
        let mut arg_ptrs: Vec<inkwell::values::PointerValue<'ctx>> = Vec::new();
        for i in 1..args.len() {
            let arg_type = arg_types.get(i).cloned().unwrap_or_default();
            let p = self.format_arg_to_cstr(&args[i], &arg_type, i)?;
            arg_ptrs.push(p);
        }
        let format_fn = self.get_runtime_fn("mimi_str_format")?;
        let num_args = arg_ptrs.len();
        let mut current = template_val;
        let mut start = 0usize;
        loop {
            let chunk_n = (num_args - start).min(8);
            let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(10);
            // First arg: number of format arguments in THIS call.
            call_args.push(BasicMetadataValueEnum::IntValue(
                i64_ty.const_int(chunk_n as u64, false),
            ));
            call_args.push(BasicMetadataValueEnum::PointerValue(current));
            for p in arg_ptrs.iter().skip(start).take(chunk_n) {
                call_args.push(BasicMetadataValueEnum::PointerValue(*p));
            }
            // Pad to the fixed 8-slot ABI with null pointers.
            while call_args.len() < 10 {
                call_args.push(BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()));
            }
            current = self
                .build_call(format_fn, &call_args, &format!("format_call_{}", start))?
                .try_as_basic_value_opt()
                .ok_or("mimi_str_format returned void")?
                .into_pointer_value();
            start += chunk_n;
            // chunk_n == 0 covers the no-substitution template case
            // (single call with count 0, matching the legacy emission).
            if chunk_n == 0 || start >= num_args {
                break;
            }
        }
        let result_ptr = current;
        // H-17: release display buffers registered by aggregate arg emitters
        // (consumed by the mimi_str_format call chain above). Safe: the
        // format result is the runtime's own allocation, not a display buffer.
        self.flush_display_since(disp_marker)?;
        // Wrap into canonical string struct {i8*, i64}
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(result_ptr)],
                "fmt_strlen",
            )
            .map_err(|e| CompileError::LlvmError(format!("format strlen: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        self.build_string_struct(result_ptr, len)
    }

    /// H-17 fix: lower a `format` substitution argument to a C string
    /// pointer, reusing the print-family type dispatch (`extract_print_arg`).
    ///
    /// Supported: strings (pass-through), scalars (int/bool via
    /// `mimi_to_string_i64`, float via `mimi_to_string_f64`), Map/Set opaque
    /// handles (runtime JSON serializers), and all aggregates
    /// (List/Record/Enum/Option/Result/Tuple) via their display emitters —
    /// matching the VM, which formats any value with its Display impl
    /// (e.g. `format("{}", [1,2,3])` → `"[1, 2, 3]"`).
    ///
    /// Anything else — including shapes with no compile-time type info that
    /// cannot be shape-detected — is a compile error, never a panic and
    /// never a silent null substitution.
    fn format_arg_to_cstr(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        arg_type: &str,
        idx: usize,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        // Scalar int (excluding Map/Set opaque handles): render via
        // `mimi_to_string_i64` with defensive width normalization.
        if let BasicMetadataValueEnum::IntValue(iv) = arg {
            let is_handle = arg_type == "Map"
                || arg_type.starts_with("Map<")
                || arg_type == "Set"
                || arg_type.starts_with("Set<")
                || arg_type == "set";
            if !is_handle {
                // VM parity: bool renders as "true"/"false", not "1"/"0".
                let is_bool = arg_type == "bool" || iv.get_type().get_bit_width() == 1;
                if is_bool {
                    let true_ptr = self
                        .builder
                        .build_global_string_ptr("true", &format!("fmt_true_{idx}"))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let false_ptr = self
                        .builder
                        .build_global_string_ptr("false", &format!("fmt_false_{idx}"))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    return Ok(self
                        .builder
                        .build_select(
                            *iv,
                            true_ptr.as_pointer_value(),
                            false_ptr.as_pointer_value(),
                            &format!("fmt_bool_ptr_{idx}"),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_pointer_value());
                }
                let to_i64_fn = self.get_runtime_fn("mimi_to_string_i64")?;
                let bw = iv.get_type().get_bit_width();
                let iv64 = if bw == 1 {
                    self.builder
                        .build_int_z_extend(*iv, i64_ty, "fmt_bool_zext")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else if bw < 64 {
                    self.builder
                        .build_int_s_extend(*iv, i64_ty, "fmt_int_sext")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    *iv
                };
                let str_ptr = self
                    .build_call(
                        to_i64_fn,
                        &[BasicMetadataValueEnum::IntValue(iv64)],
                        "to_str_i64",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_to_string_i64 returned void")?
                    .into_pointer_value();
                return Ok(str_ptr);
            }
        }
        if let BasicMetadataValueEnum::FloatValue(fv) = arg {
            let to_f64_fn = self.get_runtime_fn("mimi_to_string_f64")?;
            let str_ptr = self
                .build_call(
                    to_f64_fn,
                    &[BasicMetadataValueEnum::FloatValue(*fv)],
                    "to_str_f64",
                )?
                .try_as_basic_value_opt()
                .ok_or("mimi_to_string_f64 returned void")?
                .into_pointer_value();
            return Ok(str_ptr);
        }
        // Raw C string / plain pointer with a known non-aggregate type:
        // pass through unchanged.
        if let BasicMetadataValueEnum::PointerValue(pv) = arg {
            let is_list = arg_type.starts_with("List");
            let is_record = !arg_type.is_empty()
                && self
                    .type_defs
                    .get(arg_type)
                    .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)));
            if !is_list && !is_record {
                return Ok(*pv);
            }
        }
        // No compile-time type info (legacy codegen path): shape-detect the
        // common struct layouts so `format` keeps working there too.
        if arg_type.is_empty() {
            if let BasicMetadataValueEnum::StructValue(sv) = arg {
                let fields = sv.get_type().get_field_types();
                let is_str =
                    fields.len() == 2 && matches!(fields[0], BasicTypeEnum::PointerType(_));
                let is_list = fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::IntType(t) if t.get_bit_width() == 64)
                    && matches!(fields[1], BasicTypeEnum::PointerType(_));
                if is_str {
                    let ptr = self
                        .build_extract_value((*sv).into(), 0, "fmt_str_ptr")?
                        .into_pointer_value();
                    return Ok(ptr);
                }
                if is_list {
                    return self.emit_list_i32_to_string(*sv);
                }
            }
        }
        // Aggregates (List/Map/Set/Record/Enum/Option/Result/Tuple) and any
        // remaining shapes: reuse the print-family dispatch, which lowers
        // every supported type to a display C string ("%s"). Anything else
        // is a compile error — never a panic.
        match self.extract_print_arg(arg, i64_ty, arg_type)? {
            (BasicMetadataValueEnum::PointerValue(pv), spec) if spec == "%s" => Ok(pv),
            _ => Err(CompileError::TypeMismatch(format!(
                "format: argument {} has unsupported type '{}'",
                idx, arg_type
            ))),
        }
    }

    // === Binary I/O & streaming line reading (codegen) ===

    /// Trap with a static message via `mimi_runtime_abort`. Used when
    /// binary/stream IO runtime helpers return NULL to signal an error that
    /// the old code silently mapped to an empty string / empty array.
    fn emit_io_trap(&self, message: &str, label: &str) -> MimiResult<()> {
        let abort_fn = self.get_or_declare_abort_fn();
        let msg = self
            .builder
            .build_global_string_ptr(message, &format!("{}_msg", label))
            .map_err(|e| format!("global string error: {}", e))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(msg.as_pointer_value())],
            &format!("{}_abort", label),
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| format!("unreachable error: {}", e))?;
        Ok(())
    }

    pub(super) fn compile_read_file_partial(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "read_file_partial expects 2 arguments".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let max_bytes = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "read_file_partial: max_bytes must be i64".into(),
                ))
            }
        };
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let out_len = self.build_alloca(i64_ty, "read_file_partial_len")?;
        let func = self.get_runtime_fn("mimi_read_file_partial_ll")?;
        let raw_ptr = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::IntValue(max_bytes),
                    BasicMetadataValueEnum::PointerValue(out_len),
                ],
                "read_file_partial_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("read_file_partial error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_read_file_partial_ll returned void")?
            .into_pointer_value();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function".to_string())?;
        let is_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                raw_ptr,
                i8_ptr_ty.const_null(),
                "read_file_partial_is_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let trap_bb = self
            .context
            .append_basic_block(function, "read_file_partial_null_bb");
        let ok_bb = self
            .context
            .append_basic_block(function, "read_file_partial_ok_bb");
        self.builder
            .build_conditional_branch(is_null, trap_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("cbr error: {}", e)))?;
        self.builder.position_at_end(trap_bb);
        self.emit_io_trap(
            "read_file_partial: file not found or read failed",
            "read_file_partial_null",
        )?;
        self.builder.position_at_end(ok_bb);
        let len = self
            .build_load(i64_ty, out_len, "read_file_partial_len_val")?
            .into_int_value();
        // Build Mimi string struct {i8*, i64} directly so embedded NUL bytes
        // survive (batch4-01 P2-6).
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let str_val = self
            .builder
            .build_insert_value(string_ty.get_undef(), raw_ptr, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("insert ptr error: {}", e)))?;
        let str_val = self
            .builder
            .build_insert_value(str_val, len, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("insert len error: {}", e)))?;
        Ok(str_val.into_struct_value().into())
    }

    pub(super) fn compile_read_file_bytes(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "read_file_bytes expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let out_len = self.build_alloca(i64_ty, "read_file_bytes_len")?;
        let func = self.get_runtime_fn("mimi_read_file_bytes_ll")?;
        let raw_ptr = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(out_len),
                ],
                "read_file_bytes_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("read_file_bytes error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_read_file_bytes_ll returned void")?
            .into_pointer_value();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function".to_string())?;
        let is_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                raw_ptr,
                i8_ptr_ty.const_null(),
                "read_file_bytes_is_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let trap_bb = self
            .context
            .append_basic_block(function, "read_file_bytes_null_bb");
        let ok_bb = self
            .context
            .append_basic_block(function, "read_file_bytes_ok_bb");
        self.builder
            .build_conditional_branch(is_null, trap_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("cbr error: {}", e)))?;
        self.builder.position_at_end(trap_bb);
        self.emit_io_trap(
            "read_file_bytes: file not found or read failed",
            "read_file_bytes_null",
        )?;
        self.builder.position_at_end(ok_bb);
        let len = self
            .build_load(i64_ty, out_len, "read_file_bytes_len_val")?
            .into_int_value();
        // Build Mimi string struct {i8*, i64} directly so embedded NUL bytes
        // survive (batch4-01 P2-6).
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let str_val = self
            .builder
            .build_insert_value(string_ty.get_undef(), raw_ptr, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("insert ptr error: {}", e)))?;
        let str_val = self
            .builder
            .build_insert_value(str_val, len, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("insert len error: {}", e)))?;
        Ok(str_val.into_struct_value().into())
    }

    pub(super) fn compile_write_file_bytes(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "write_file_bytes expects 2 arguments".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let (data_ptr, data_len) = self.extract_raw_str_ptr_len(&args[1])?;
        let func = self.get_runtime_fn("mimi_write_file_bytes_ll")?;
        let result = self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                    BasicMetadataValueEnum::IntValue(data_len),
                ],
                "write_file_bytes_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("write_file_bytes error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_write_file_bytes_ll returned void")?
            .into_int_value();
        let zero = self.context.i32_type().const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                result,
                zero,
                "write_file_bytes_ok",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        Ok(cmp.into())
    }

    pub(super) fn compile_read_lines_json(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "read_lines_json expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let func = self.get_runtime_fn("mimi_read_lines_json")?;
        let raw_ptr = self
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                "read_lines_json_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("read_lines_json error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_read_lines_json returned void")?
            .into_pointer_value();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function".to_string())?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let is_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                raw_ptr,
                i8_ptr_ty.const_null(),
                "read_lines_json_is_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let trap_bb = self
            .context
            .append_basic_block(function, "read_lines_json_null_bb");
        let ok_bb = self
            .context
            .append_basic_block(function, "read_lines_json_ok_bb");
        self.builder
            .build_conditional_branch(is_null, trap_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("cbr error: {}", e)))?;
        self.builder.position_at_end(trap_bb);
        self.emit_io_trap(
            "read_lines_json: file not found or read failed",
            "read_lines_json_null",
        )?;
        self.builder.position_at_end(ok_bb);
        self.wrap_c_string(raw_ptr)
    }

    pub(super) fn compile_exec_safe(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // exec_safe(prog, arg1, arg2, …) → mimi_exec_safe(prog, MimiList* argv).
        // Pack remaining string args into a temporary {len, data} list (null
        // when no extra args). Matches interpreter varargs semantics.
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "exec_safe expects at least 1 argument (program)".to_string(),
            ));
        }
        let prog_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let mut argv_data: Option<inkwell::values::PointerValue<'ctx>> = None;
        let args_list = if args.len() == 1 {
            i8_ptr.const_null()
        } else {
            // Pack argv[1..] as C-string pointers into a MimiList on the stack.
            let n = (args.len() - 1) as u64;
            let n_iv = i64_ty.const_int(n, false);
            let ptr_size = i64_ty.const_int(8, false);
            let data_bytes = self
                .builder
                .build_int_mul(n_iv, ptr_size, "exec_argv_bytes")
                .map_err(|e| CompileError::LlvmError(format!("mul: {}", e)))?;
            let data_raw = self.malloc_or_abort(data_bytes, "exec_argv_data")?;
            for (i, arg) in args.iter().skip(1).enumerate() {
                let s_ptr = self.extract_raw_str_ptr(arg)?;
                let idx = i64_ty.const_int(i as u64, false);
                let slot = self
                    .gep()
                    .build_in_bounds_gep(
                        BasicTypeEnum::PointerType(i8_ptr),
                        data_raw,
                        &[idx],
                        &format!("exec_argv_{}", i),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.build_store(slot, s_ptr)?;
            }
            let list_ty = self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            let list_alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "exec_argv")?;
            self.build_store(
                self.gep()
                    .build_struct_gep(BasicTypeEnum::StructType(list_ty), list_alloca, 0, "alen")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
                n_iv,
            )?;
            self.build_store(
                self.gep()
                    .build_struct_gep(BasicTypeEnum::StructType(list_ty), list_alloca, 1, "adata")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
                data_raw,
            )?;
            argv_data = Some(data_raw);
            list_alloca
        };
        let exec_fn = self.get_runtime_fn("mimi_exec_safe")?;
        let res_ptr = self
            .build_call(
                exec_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(prog_ptr),
                    BasicMetadataValueEnum::PointerValue(args_list),
                ],
                "exec_safe_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("exec_safe error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_exec_safe returned void")?
            .into_pointer_value();
        // The argv data array is only a transient marshalling buffer:
        // mimi_exec_safe consumes the list synchronously, so it can be freed
        // immediately after the call (batch4-01 P2-2).
        if let Some(argv_data) = argv_data {
            let free_fn = self.get_runtime_fn("free")?;
            self.build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(argv_data)],
                "exec_safe_free_argv",
            )?;
        }

        // Reuse the same MimiExecResult → ExecResult lowering as compile_exec.
        let res_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        );
        let exit_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 0, "exit_code_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let exit_code_raw = self
            .build_load(self.context.i64_type(), exit_gep, "exit_code_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let exit_code = self
            .builder
            .build_int_truncate(exit_code_raw, self.context.i32_type(), "exit_code_i32")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
        let stdout_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 1, "stdout_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stdout_raw = self
            .build_load(i8_ptr, stdout_gep, "stdout_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stdout_str = self.wrap_c_string(stdout_raw)?;
        let stderr_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 2, "stderr_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stderr_raw = self
            .build_load(i8_ptr, stderr_gep, "stderr_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stderr_str = self.wrap_c_string(stderr_raw)?;
        let free_struct_fn = self.get_runtime_fn("mimi_exec_free_struct")?;
        self.build_call(
            free_struct_fn,
            &[BasicMetadataValueEnum::PointerValue(res_ptr)],
            "exec_safe_free_struct",
        )?;
        let string_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let exec_result_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i32_type()),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(exec_result_ty, "exec_safe_result")?;
        let f0 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 0, "es_f0")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(f0, exit_code)?;
        let f1 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 1, "es_f1")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(f1, stdout_str)?;
        let f2 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 2, "es_f2")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(f2, stderr_str)?;
        self.build_load(exec_result_ty, alloca, "exec_safe_val")
    }

    pub(super) fn compile_exec_pipe(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "exec_pipe expects 1 argument".to_string(),
            ));
        }
        let cmd_ptr = self.extract_raw_str_ptr(&args[0])?;
        let func = self.get_runtime_fn("mimi_exec_pipe")?;
        let raw_ptr = self
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(cmd_ptr)],
                "exec_pipe_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("exec_pipe error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_exec_pipe returned void")?
            .into_pointer_value();
        self.wrap_c_string(raw_ptr)
    }
}
