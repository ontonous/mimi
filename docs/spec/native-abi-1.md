# Mimi Native ABI 1

> Normative profile: `mimi-native-abi-1`
> Binding source: `docs/language-requirements.toml`. This header is descriptive; the manifest is authoritative.

## 1. 范围

只用于同进程 Component transport。公开 surface 由 `.mimiabi` 生成；raw C ABI 仅存在于 generated shim 或显式 unsafe/experimental adapter。

## 2. 数据类别

- fixed-width integer 和显式 bool；
- UTF-8 `{ptr,len}` view；
- owned string/buffer 携带 allocator/destructor identity；
- slice/mutate slice 在调用动态范围内有效，mutate 不得 realloc；
- POD 使用冻结的 size/align/offset/tag；
- 非 POD、Flow、Actor、Session、capability、callback 使用 nominal handle。

不暴露 Rust enum/Result/Vec、Mimi internal Value、裸 Flow payload 或 `void*` fallback。

## 3. Handle 与 lease

逻辑 token 包含 kind、type/protocol ID、slot、generation、runtime instance 和 permission。每个入口完整校验。释放 slot 提升 generation，回绕前退休 slot。

lookup 获得 lease：`Alive -> Closing -> Dead`。Closing 拒绝新 lease；最后一个 in-flight lease 结束后才物理释放。Child/view handle 绑定 parent slot+generation。

## 4. Result 和 ownership

Boundary result 使用固定宽度 tag 和 append-only registry。Out payload 在失败前保持初始化安全；unknown tag fail-closed。Owned payload 只能由匹配的 generated destructor 释放，allocator provenance 不得跨 CRT/runtime 混用。

## 5. Callback 与 async

Scoped callback 不得逃逸；subscription close 必须等待 foreign quiescence 并 drain in-flight call。Async task 为线性状态机，cancel request 不等于 completion，且恰好一个 terminal outcome。确认 quiescence 前不释放 borrow、pin、callback 或 capability。

## 6. 握手与演进

初始化交换 ABI major/minor、layout hash、pointer width、endianness、calling convention、Protocol IDs、allocator ABI 和 capability bitmap。Major 不兼容拒绝加载；minor 只允许规范定义的尾部追加。所有 struct/VTable 以 size 开头，双方只访问共同前缀。

## 7. 布局冻结声明（0.34.45，ADR-007 落地）

> 裁决：**冻结当前值表示布局为 1.0 ABI**（architecture-freeze AF-2）。1.x 迁移通道用
> ABI 版本握手保留为 Two-Way Door（见 §8）。本声明是部署合同：任何组件一旦发布，
> 其观察到的布局即视为 1.0 兼容面的一部分。

### 7.1 冻结布局定义

| 值类别 | 布局 | 语义 |
|--------|------|------|
| fixed-width int / bool | 显式宽度整数 | 无 tag、无冗余表示；i32/i64 宽度语义见 language-spec §数值 |
| string view | `{ptr: ptr, len: i64}` | UTF-8 **无 capacity** 视图；len ≠ 0 时 ptr 必须有效；`len=0` 时 ptr 可为悬垂（允许 `{0,0}`） |
| owned string/buffer | `{ptr, len}` + allocator/destructor identity | 只能由匹配的 generated destructor 释放；allocator provenance 不得跨 CRT/runtime 混用 |
| slice / mutate slice | `{ptr, len}` | 仅在调用动态范围内有效；mutate 不得 realloc（§2） |
| list（heap 容器） | 数据指针 + `has_header` 显式标志 | **禁止裸读 `data[-1]`**：容量/长度信息只能通过 flags 或显式 header 访问（B10 缓解，fail-closed） |
| scalar handle（Result/Option/非 POD） | i64 位宽 + tag 位 | tag 位**永驻**（A3）：`any_value_to_handle` 不剥离 tag；识别只能按 tag 字面量，不得假设无 tag 表示 |
| nominal handle（Flow/Actor/Session/capability/callback） | slot+generation | 见 §3 lease 语义；generation 回绕前退休 slot |

### 7.2 布局内解决的约束清单

以下债务**不阻塞 1.0**，但作为 ABI 约束写死——任何实现必须在此布局内工作，不得
产生布局外依赖：

| 编号 | 约束 | 处置 |
|------|------|------|
| **A2** | `ptrtoint`/`inttoptr` 指针来源丢失（-O2 静默误编译） | 布局内解决：handle 化路径禁止指针↔整数往返；LLVM pointer provenance 方案在 1.x 评估（非 Mimi 单方可修） |
| **A3** | handle tag 位永驻 | 布局内解决：tag 永驻是正式语义（§7.1）；NaN-boxing/fat value 重设计改布局 → 新 abi_version（§8） |
| **B10** | 隐式 list 容量头 `data[-1]` | 布局内解决：`has_header` 显式标志 + fail-closed 拒绝无头裸读；胖指针 `{data,len,capacity}` 改布局 → 新 abi_version（§8） |

## 8. ABI 版本握手登记（0.2 实施）

- 组件身份携带 `abi_version: u32`（`ComponentIdentity`，当前值 **1**，对应本文件布局）；
- 握手协商：双方 `abi_version` 相等才可加载；major 不兼容拒绝（§6）；
- **Two-Way Door**：1.x 任何布局变更（胖指针、tag 剥离、list capacity）→ 新 `abi_version`，
  旧组件继续以旧版本加载，迁移工具按版本对编译产物分类；
- 0.2 随 bindgen 回归铁律一并实施（architecture-freeze AF-2 登记）。
