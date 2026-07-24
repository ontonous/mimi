# v0.31 Component Boundary

## 版本

- **0.31.27**：Component IR / `.mimiabi`，所有 generator 只消费同一 IR。
- **0.31.28**：Native ABI、allocator provenance、typed generational handle 和 concurrent lease。
- **0.31.29**（新增）：Component 稳定检查点 — ABI fuzz + handle race + 回归扫描，0 新 surface。
- **0.31.30**：Subscription quiescence、async cancel exactly-one terminal。
- **0.31.31**：canonical Wire Schema、limits、handshake、revision/conflict、replay。
- **0.31.32**：Rust Safe SDK。
- **0.31.33**：XPU FFI 验证 — extern "C" + #[repr(C)] 调通真实 C 库（OpenVINO/Level Zero/任意 .so），**全生命周期 E2E 闭环**（v2 升级），Z3 验证指针非空 + ASan 零泄漏。
- **0.31.34**：Component 攻击审查，0 新 surface。
- **0.31.35**（新增）：SDK conformance 加固 — Rust SDK E2E + XPU FFI 边界 + Wire fuzz，0 新 surface。

## 0.31.29 稳定检查点详情

> 插入原因：0.31.27–27 从零建立 Component IR 和 Native ABI，生成大量新代码。
> 在 Callback/Async/Wire 等上层机制构建前，确认 ABI 基础稳固。

### 交付

1. **ABI fuzz**：
   - C/Rust `size`/`align`/`offset`/`tag` probe 跨平台（x86_64 + aarch64）
   - `.mimiabi` 格式 round-trip：serialize → deserialize → 语义等价
   - 版本矩阵：当前 IR 版本 × 前向兼容策略

2. **Handle lease race**：
   - 并发 acquire/release/expiry 测试（多线程 stress）
   - ABA 检测：generation 递增，旧 handle 不可复用
   - wrong-kind/wrong-runtime 检测：handle 类型不匹配 → 运行时错误

3. **Allocator mismatch**：
   - 跨边界 alloc/free 配对检查
   - allocator provenance 标记：哪侧分配，哪侧释放

4. **回归扫描**：
   - 0.31.27–27 变更的全量测试
   - 不新增 Component surface

### 门禁

- C/Rust size/align/offset probe 全绿
- ABA、wrong-kind/runtime 检测通过
- handle lease 并发 stress 0 race（TSan 或等价）
- 全量测试连续两次绿

## 0.31.35 SDK conformance 加固详情

> 插入原因：Rust SDK（0.31.32）和 XPU FFI（0.31.33）各自 E2E 通过后，
> 需要验证跨语言交互的边界情况。"各自能跑" ≠ "一起能跑"。

### 交付

1. **Rust SDK 边界**：
   - cancel/complete race：async task 取消与完成同时到达
   - late callback delivery：subscription close 后的迟到回调
   - lease expiry：handle 超时后的操作 → BoundaryResult 错误

2. **XPU FFI 边界**：
   - null 返回：C 函数返回 NULL 时 Mimi 侧 Result::Err 正确传播
   - 错误码：C 侧 errno/错误码 → Mimi Result 映射
   - 大结构体：>64 字节的 #[repr(C)] 结构体跨边界传递
   - 字符串生命周期：C 侧分配的字符串谁释放

3. **Wire 边界**：
   - schema bomb：超大 payload / 深度嵌套
   - unknown field/tag：前向兼容（忽略未知字段）
   - replay：重复消息检测
   - duplicate/out-of-order：乱序消息处理
   - limits fuzz：超出声明限制的消息

4. **全链路 E2E**：
   - Rust adapter ↔ Mimi runtime ↔ C 库 完整 round-trip
   - 包含正常路径 + 至少 3 个错误路径

### 门禁

- Rust SDK E2E 全绿
- XPU FFI E2E 全绿
- Wire fuzz 0 crash、0 未处理错误
- 全链路 round-trip 通过（正常 + 错误路径）
- 全量测试连续两次绿

## 不变量

- Raw extern 只属于显式 unsafe/experimental adapter。
- Stable surface 不暴露 internal Value、Rust layout、void* fallback 或裸 integer handle。
- Native token、pointer、allocator、callback ctx 永不进入 wire。
- GUI 只持 projection/speculative state，不持业务提交权。
- **隐式 JSON 回退在 0.31.40 后移除**（v2 盲审修正）：复杂类型跨边界必须显式 `#[abi(json)]` 或使用 ffi slice/handle 模式。
- **函数名 errno 猜测在 0.31.22 后废除**（v2 盲审修正）：外部 import 必须显式声明 `#[abi(errno(...))]`。
- **fork() 崩溃隔离在 Component 阶段移除**（v2 盲审修正）：C 库崩溃直接传播，真正的隔离通过 Wire Schema / 进程隔离实现。

## 0.31.33 XPU FFI 全生命周期 E2E 详情（v2 升级）

> 盲审修正：原"调通一个真实 C 库函数"范围不足。必须打通全生命周期闭环。

### 交付

1. **全生命周期 E2E**：
   - **Create**：调用 C 库初始化函数，返回 `#[repr(C)]` 句柄/上下文
   - **Transfer**：C 侧分配的内存所有权转移到 Mimi（或 Mimi 分配传给 C）
   - **Compute**：用句柄执行实际计算（非 trivial 操作）
   - **Drop/Free**：Mimi 侧 drop 触发 C 侧 cleanup，资源完全释放

2. **验证**：
   - Z3 验证指针非空（`requires: ptr != 0`）
   - ASan 验证零泄漏（E2E 结束后无 dangling allocation）
   - 结构体偏移与 C 侧 `_Static_assert(offsetof(...))` 一致
   - 双后端等价（interp 跳过 FFI，codegen 执行）

3. **错误路径**：
   - C 函数返回 NULL → Mimi `Result::Err` 正确传播
   - C 函数设置 errno → Mimi `Result::Err` 正确映射
   - 中途失败 → 已分配资源正确释放（无泄漏）

### 门禁

- ≥1 个真实 C 库全生命周期 E2E 通过（`mimi build && ./output`）
- ASan 零泄漏、零 UAF
- Z3 指针非空验证通过
- 结构体偏移与 C 侧一致
- 错误路径 ≥3 个通过

## 门禁

- C/Rust size/align/offset/tag probe、ABI diff 和版本矩阵。
- wrong-kind/runtime、ABA、lease race、allocator mismatch。
- callback self-close/late delivery、cancel/complete race。
- schema bomb、unknown field/tag、replay、duplicate/out-of-order、limits fuzz。
- Rust adapter 与 XPU FFI 真实 E2E。
