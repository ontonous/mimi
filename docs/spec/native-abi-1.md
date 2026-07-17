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
