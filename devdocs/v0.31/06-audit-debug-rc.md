# v0.31 止血、DEBUG、审查与 RC

## 专用版本

- **0.31.6 止血 I**：语义中枢回归。
- **0.31.18 地基深修 III**：证据同步 + 回归扫描（stabilization）。
- **0.31.19 攻击审查 I**：Flow/Actor/Session/resource 线性（基于闭环地基）；追加 B：性能 quick wins。
- **0.31.20 Runtime Efficiency**：解释器热路径 dispatch 重构 + Value clone 消减 + LLVM O1 默认 + 性能基线 CI。
- **0.31.26 止血/审查 II**：Verifier。
- **0.31.29 Component 稳定检查点**：ABI fuzz + handle race + 回归。
- **0.31.34 Component 审查**：ABI/Wire/callback/async。
- **0.31.35 SDK conformance 加固**：双 SDK E2E + Wire fuzz。
- **0.31.42 DEBUG**：组合 fuzz、MCDD reference model、性能/内存、flake、standalone binary strip 验证。
- **0.31.43 最终敌对审查**：P0、逃生口、silent fallback、unsupported warning。
- **0.31.44 RC1**：冻结全部 stable profile，只修阻断缺陷。
- **0.31.45 RC2**：第二次干净环境、跨平台、迁移和 SDK E2E。

## 基线门禁顺序

```bash
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test --no-run
ulimit -v 20000000 && LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test -- --test-threads=1
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test v1_2_verification -- --test-threads=1
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
python3 scripts/check_language_docs.py
python3 scripts/check_v031_roadmap.py
python3 tests/real_world/run_suite.py
```

ASan/UBSan/Valgrind/TSan/Miri 分开运行。工具不可用记为未执行，不能算通过。

## 发布规则

- 目标门禁连续两次全绿后才 tag。
- 审查版不得新增 stable feature。
- P0 必须为 0；P1 必须有 owner 和 deadline。
- ignored 不得增长；功能性 ignored 在对应版本清零。
- 发现 silent miscompilation、UAF 或 false proof 时立即停止发布线。
