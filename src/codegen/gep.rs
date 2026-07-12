use inkwell::builder::{Builder, BuilderError};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{IntValue, PointerValue};

/// CheckedGepBuilder: 安全的 GEP API 抽象
///
/// v0.17 GEP 安全重构的核心抽象，封装 inkwell Builder 的 build_gep/build_struct_gep，
/// 确保所有指针索引操作经过安全检查。
///
/// # 安全性保证 (M12 deep audit correction)
/// - `build_gep`: indices MAY be runtime values (not compile-time constants).
///   Callers MUST ensure in-bounds access. This does NOT insert bounds checks.
/// - `build_in_bounds_gep`: `inbounds` is an OPTIMIZER HINT, NOT a bounds check.
///   It does NOT insert trap IR or runtime bounds checks. Out-of-bounds is silent UB.
///   Callers MUST ensure indices are in-bounds for the pointed-to allocation.
/// - `build_struct_gep`: delegates to inkwell's API; field_index must be valid.
pub struct CheckedGepBuilder<'a, 'ctx> {
    builder: &'a Builder<'ctx>,
}

impl<'a, 'ctx> CheckedGepBuilder<'a, 'ctx> {
    pub fn new(builder: &'a Builder<'ctx>) -> Self {
        Self { builder }
    }

    /// GEP with indices that MAY be runtime-determined (absorb unsafe).
    /// Callers MUST ensure indices are in-bounds for the pointed-to allocation.
    pub fn build_gep<T: Into<BasicTypeEnum<'ctx>>>(
        &self,
        result_type: T,
        ptr: PointerValue<'ctx>,
        indices: &[IntValue<'ctx>],
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError> {
        // SAFETY:
        // - ptr comes from a valid LLVM-typed allocation (alloca/malloc/global)
        // - indices are i64 values; callers guarantee they are in-bounds
        // - result_type matches the pointer's pointed-to type
        // - This is NOT a bounds check — out-of-bounds indices are UB.
        unsafe {
            self.builder
                .build_gep(result_type.into(), ptr, indices, name)
        }
    }

    /// Inbounds GEP — `inbounds` is an optimizer hint, NOT a bounds check.
    /// It does NOT insert trap IR. Out-of-bounds access is silent UB.
    /// Callers MUST ensure indices are in-bounds.
    pub fn build_in_bounds_gep<T: Into<BasicTypeEnum<'ctx>>>(
        &self,
        result_type: T,
        ptr: PointerValue<'ctx>,
        indices: &[IntValue<'ctx>],
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError> {
        // SAFETY:
        // - ptr comes from a valid LLVM-typed allocation
        // - indices are i64 values; callers guarantee in-bounds
        // - result_type matches the pointer's pointed-to type
        // - `inbounds` keyword tells the optimizer the result stays within
        //   the allocation. It does NOT insert runtime bounds checks or traps.
        //   Violation is undefined behavior, not a trap.
        unsafe {
            self.builder
                .build_in_bounds_gep(result_type.into(), ptr, indices, name)
        }
    }

    /// 结构体字段访问 GEP（inkwell 安全 API）。
    pub fn build_struct_gep<T: Into<BasicTypeEnum<'ctx>>>(
        &self,
        struct_type: T,
        ptr: PointerValue<'ctx>,
        field_index: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError> {
        // SAFETY: 结构体 GEP 由 inkwell 的安全 API 保证
        self.builder
            .build_struct_gep(struct_type.into(), ptr, field_index, name)
    }
}
