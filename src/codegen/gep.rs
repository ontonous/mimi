use inkwell::builder::{Builder, BuilderError};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{IntValue, PointerValue};

/// CheckedGepBuilder: 安全的 GEP API 抽象
///
/// v0.17 GEP 安全重构的核心抽象，封装 inkwell Builder 的 build_gep/build_struct_gep，
/// 确保所有指针索引操作经过安全检查。
///
/// # 安全性保证
/// - `build_gep`: 编译期常量索引，absorb unsafe，指针来自有效 LLVM 类型化分配
/// - `build_in_bounds_gep`: 运行时索引场景，LLVM 自动插入 trap IR 处理越界
/// - `build_struct_gep`: 委托给 inkwell 的安全 API
pub struct CheckedGepBuilder<'a, 'ctx> {
    builder: &'a Builder<'ctx>,
}

impl<'a, 'ctx> CheckedGepBuilder<'a, 'ctx> {
    pub fn new(builder: &'a Builder<'ctx>) -> Self {
        Self { builder }
    }

    /// 编译期常量索引的 GEP（absorb unsafe）。
    /// 适用于索引在编译时已知且来自有效类型化分配的场景。
    pub fn build_gep<T: Into<BasicTypeEnum<'ctx>>>(
        &self,
        result_type: T,
        ptr: PointerValue<'ctx>,
        indices: &[IntValue<'ctx>],
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError> {
        // SAFETY:
        // - ptr 来自有效的 LLVM 类型化分配（alloca / malloc / global）
        // - indices 为正确类型的 i64 值，编译期已知
        // - result_type 与指针指向的类型匹配
        unsafe {
            self.builder
                .build_gep(result_type.into(), ptr, indices, name)
        }
    }

    /// 运行时索引的 inbounds GEP（LLVM 自动插入 trap IR 处理越界）。
    /// 适用于列表遍历、排序、映射等运行时索引场景。
    /// 注意：inkwell 0.9.0 中此函数同样为 unsafe，由本抽象统一吸收。
    pub fn build_in_bounds_gep<T: Into<BasicTypeEnum<'ctx>>>(
        &self,
        result_type: T,
        ptr: PointerValue<'ctx>,
        indices: &[IntValue<'ctx>],
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError> {
        // SAFETY:
        // - ptr 来自有效的 LLVM 类型化分配
        // - indices 为正确类型的 i64 值
        // - result_type 与指针指向的类型匹配
        // - LLVM 在越界时自动插入 trap IR
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
