# v0.31 Component Boundary

## 版本

- **0.31.21**：Component IR / `.mimiabi`，所有 generator 只消费同一 IR。
- **0.31.22**：Native ABI、allocator provenance、typed generational handle 和 concurrent lease。
- **0.31.23**：Subscription quiescence、async cancel exactly-one terminal。
- **0.31.24**：canonical Wire Schema、limits、handshake、revision/conflict、replay。
- **0.31.25**：Rust Safe SDK。
- **0.31.26**：TypeScript GUI SDK 与 authority/revision 模型。
- **0.31.27**：Component 攻击审查，0 新 surface。

## 不变量

- Raw extern 只属于显式 unsafe/experimental adapter。
- Stable surface 不暴露 internal Value、Rust layout、void* fallback 或裸 integer handle。
- Native token、pointer、allocator、callback ctx 永不进入 wire。
- GUI 只持 projection/speculative state，不持业务提交权。

## 门禁

- C/Rust size/align/offset/tag probe、ABI diff 和版本矩阵。
- wrong-kind/runtime、ABA、lease race、allocator mismatch。
- callback self-close/late delivery、cancel/complete race。
- schema bomb、unknown field/tag、replay、duplicate/out-of-order、limits fuzz。
- Rust adapter 与 TS GUI 真实 E2E。
