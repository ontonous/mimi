# v0.31 止血、DEBUG、审查与 RC

## 专用版本

- **0.31.6 止血 I**：语义中枢回归。
- **0.31.16 攻击审查 I**：Flow/Actor/Session/resource。
- **0.31.21 止血/审查 II**：Verifier。
- **0.31.28 Component 审查**：ABI/Wire/callback/async。
- **0.31.34 DEBUG**：组合 fuzz、MCDD reference model、性能/内存、flake。
- **0.31.35 最终敌对审查**：P0、逃生口、silent fallback、unsupported warning。
- **0.31.36 RC1**：冻结全部 stable profile，只修阻断缺陷。
- **0.31.37 RC2**：第二次干净环境、跨平台、迁移和 SDK E2E。

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
