# Mimi 开发者工作流与代码库梳理报告

> **最后更新**：2026-11-14（工作分支：`codex/0.1.11-closure-to-0.2-dev`，包版本：`0.1.11-dev`）
> **目标**：把原本分散、相互矛盾的多套环境脚本、门禁口径、文档入口和测试配置收敛成**单源统一、快速反馈、不丢未提交 WIP 现场**的开发流程，并沉淀下一步高杠杆重构清单。

---

## 1. 统一日常开发工作流（本地 ↔ CI 单源对齐）

仓库不再要求开发者手写 `LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper` 或猜不同脚本里的参数组合。所有常规构建、静态门禁与测试都收敛到三个协同入口：

- 环境引导：[`scripts/dev-env.sh`](../scripts/dev-env.sh)
  自动探测 `/tmp/llvm-wrapper/bin/llvm-config`（LLVM 18）；若不存在或版本不匹配，自动调用 [`scripts/setup-llvm-wrapper.sh`](../scripts/setup-llvm-wrapper.sh) 初始化，并导出 `LLVM_SYS_181_PREFIX`。已被 [`Makefile`](../Makefile)、[`scripts/run-gates.sh`](../scripts/run-gates.sh)、[`scripts/check_all_gates.sh`](../scripts/check_all_gates.sh)、[`scripts/run-ci-matrix.sh`](../scripts/run-ci-matrix.sh)、[`scripts/perf-gate.sh`](../scripts/perf-gate.sh) 与 [`scripts/fuzz-common.sh`](../scripts/fuzz-common.sh) 共用。
- 命令入口：[`Makefile`](../Makefile)（`make help` 查看全清单）
- 分层门禁执行器：[`scripts/run-gates.sh`](../scripts/run-gates.sh)（本地与 [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) 共用）

### 1.1 分层门禁与日常命令

| 层级 | 命令 | 覆盖范围与定位 |
|---|---|---|
| Tier 0 环境与编译 | `make setup` / `make check` / `make fmt` | 引导 LLVM 18 wrapper、执行 `cargo check --locked --all-targets`、格式化代码 |
| Tier 1 秒级离线门禁 | `make gates-fast`（`bash scripts/run-gates.sh --fast`） | `check_language_docs.py`、`check_v031_roadmap.py`、`check_edge_isolation.py`、`mms-consistency.sh`、`check_plan.py --ready`、`audit-mir-native-production.sh --self-test`、`--python-core`（15 组 / 179 项离线 Python 工具/契约单测，详见 [`scripts/run-gates.sh`](../scripts/run-gates.sh)）；加 `--ci` 会追加 `--allow-missing-external` 以兼容无本地外部盘挂载的 CI runner |
| Tier 2 CI 级 Lint 仪表盘 | `make gates`（`bash scripts/run-gates.sh --lint`） | 在 keep-going 模式下完整汇总 Tier 1 + `check_unsafe_safety.py` + `audit-mir-native-production.sh` + `audit-mir-legacy-owners-static.sh` + `cargo fmt -- --check` + `cargo clippy --locked --all-targets -- -D warnings`；加 `--fail-fast` 为快速阻断模式（`make ci-check` / CI 风格） |
| Tier 3 Rust 测试与 CI 对齐 | `make test-quick` / `make test` / `make test-z3` / `make ci` | `dual_` + `typecheck::` + `ffi_` 快速烟测；`make test` 自动检测 `cargo-nextest`（使用 [`.config/nextest.toml`](../.config/nextest.toml) 进程隔离与超时配置）；`make test-z3` 强制 `--test-threads=1` 隔离 Z3 arena 内存累积；`make ci` 串联 `ci-check`、`test-quick`、`test-z3`、`ci-test` 与 `perf-gate.sh` |

---

## 2. 文档权威地图（Single Source of Truth）

仓库长期演化积累了多代规划（`0.1.0`–`0.1.11` 与 `0.2`）与快照文档。判断事实与规范时遵循下表：

| 主题 | 当前权威文件 | 备注与已知漂移 |
|---|---|---|
| 包版本 / Rust MSRV / Feature | [`Cargo.toml`](../Cargo.toml) | `0.1.11-dev`，Rust `1.88`，`llvm-sys = 181.3.0`，`z3 = 0.20.0`，`inkwell = 0.9` |
| 本地工作流与 CI 门禁 | [`Makefile`](../Makefile)、[`scripts/run-gates.sh`](../scripts/run-gates.sh)、[`.github/workflows/ci.yml`](../.github/workflows/ci.yml)、[`CONTRIBUTING.md`](../CONTRIBUTING.md) | `.github/workflows/release.yml` 复用 `ci.yml` 与 `.github/actions/setup-mimi/` |
| AI 编程与项目实现契约 | [`devdocs/kernel-card.md`](../devdocs/kernel-card.md)、[`AGENTS.md`](../AGENTS.md) | `AGENTS.md` 为本地 gitignored 文档，必须保留对 `devdocs/v0.36/philosophy-anchor.md` 的挂载引用（`check_language_docs.py` 检查） |
| 语言规范与文法权威 | [`docs/language-spec.md`](../docs/language-spec.md)、[`docs/syntax-reference.md`](../docs/syntax-reference.md)、[`docs/language-requirements.toml`](../docs/language-requirements.toml) | [`docs/language-support.toml`](../docs/language-support.toml) 头部仍写 `0.1.9-dev (internal sprint 0.39.128)`，待里程碑统一刷新 |
| 0.2 规划与 0.1.11 交接门禁 | [`devdocs/V0.2/README.md`](../devdocs/V0.2/README.md)、[`devdocs/V0.2/09-0.1.11交接门禁.md`](../devdocs/V0.2/09-0.1.11交接门禁.md)、[`devdocs/V0.2/plan.toml`](../devdocs/V0.2/plan.toml) | 通过 `make plan-check`（`devdocs/V0.2/tools/check_plan.py --ready`）校验结构与链接 |
| 运行时与 ABI 架构 | [`ARCHITECTURE.md`](../ARCHITECTURE.md) | C ABI runtime 布局、RC、`TransitionEpoch` 与 handle 租赁模型 |
| 已完成或历史参考材料 | `devdocs/archive/`、[`devdocs/v0.41/README.md`](../devdocs/v0.41/README.md)、[`gramma/mimi-grammar-v1.0.0-rc.md`](../gramma/mimi-grammar-v1.0.0-rc.md) | 只作历史/战时对照；文法已由 `docs/syntax-reference.md` 与 `devdocs/v0.34/golden/syntax-reference.golden.md` 取代 |

---

## 3. 已完成梳理与当前 WIP 实测门禁状态

### 3.1 本轮流程与工具链修复摘要

1. **门禁与测试脚本修复**：
   - [`devdocs/V0.2/tools/check_plan.py`](../devdocs/V0.2/tools/check_plan.py)：支持 `<...>` 包裹的 CommonMark 链接及 `--allow-missing-external` 参数，`make plan-check` 与 `test_v02_evidence.py` 恢复绿灯。
   - [`scripts/audit-mir-native-production.sh`](../scripts/audit-mir-native-production.sh)：为 `module_references()` 补齐 `#[cfg(test)]` 作用域下的 `include!("...")` 传播（消除 `ordinary_map_owned_any_v2.rs` 测试碎片误报，误报数由 84 降至 43），并新增 `--self-test` 中的 `include_scope` 回归用例。
   - Python 单测发现与导入修复：移除过时重复的 `scripts/test_full_route_census.py`（已保留 SHA-256 锚定的 [`tests/scripts/test_full_route_census.py`](../tests/scripts/test_full_route_census.py)），修复 [`scripts/test_dispatch_stat.py`](../scripts/test_dispatch_stat.py) 及 `tests/scripts/test_{gen_stdlib_matrix_maps,generate_route_census_authority,ordinary_map_any_v2_l3_runner_contract,render_special_route_authorities}.py` 的 `sys.path`/导入引导与契约漂移（`python-core-tests` 收编至 15 组 / 179 项离线单测并全绿）。
   - **默认 2MB 测试栈溢出消除**：在 `src/core/mir/{mod,managed_any,lower,receipt,reference,route,tests}.rs` 中将 `MirOwnershipSummary` 内 18 个大型可选收据与 `MirCheckerManagedAnySeal::branch_local_set_diamond_v2_snapshot` 改为堆上 `Box` 分配（`MirFunction` 栈帧体量由 `62,008 B` 降至 `1,936 B`，`MirOwnershipSummary` 由 `61,424 B` 降至 `1,352 B`，`MirCheckerManagedAnySeal` 由 `24,120 B` 降至 `6,808 B`），彻底解决无需 `RUST_MIN_STACK` 即可跑完整套 Rust 测试的问题。
   - **审计回归测试隔离与清理**：修复 [`src/tests/audit_fix_scripts.rs`](../src/tests/audit_fix_scripts.rs) 中 `legacy_owner_closed_route_guard_drift_fail_closed` 通过 `src` symlink 反向污染工作区 [`src/codegen/tests.rs`](../src/codegen/tests.rs) 的隔离缺陷（改为 `mirror_source_tree_except` 副本），并修复 `legacy_owner_audit_concurrent_failure_snapshot_is_path_ordered` 跨位数路径字典序排序竞态及两条过期审计字符串契约。
2. **杂物清理与本地配置收口**：
   - 删除根目录无引用的临时测试碎片 `.g2.mimi`、`.s2.mimi`、`mm_u.txt` 及 3 份已被 `-r7.sh` 取代的未跟踪 `verify-ordinary-map-v2-checked-get-l1l2*.sh` 旧草稿（原件全部备份在对话 scratch 目录 `pre_cleanup_backup/`）。
   - 更新 [`.claude/settings.local.json`](../.claude/settings.local.json)（清除过时的 `KAIROSCOPE` 绝对路径与 `LLVM_SYS_180_PREFIX`，收敛为 `make` / `run-gates.sh` / `LLVM_SYS_181_PREFIX` 白名单），并将 `.claude/` 与 `.codex*/` 纳入 [`.gitignore`](../.gitignore)。
   - 统一更新 [`Makefile`](../Makefile)、[`.github/workflows/ci.yml`](../.github/workflows/ci.yml)、[`CONTRIBUTING.md`](../CONTRIBUTING.md) 与 [`AGENTS.md`](../AGENTS.md)。

### 3.2 当前 `codex/0.1.11-closure-to-0.2-dev` WIP 实时门禁账本（2026-11-14）

- **`make check`（`cargo check --locked --all-targets`）**：**通过**（exit `0`）。
- **`make gates-fast`（Tier 1 离线门禁）**：**7/7 PASS**（含 `python-core-tests` 15 组 / 179 项离线测试全部通过）。
- **`make gates`（Tier 2 Keep-Going 仪表盘）**：**12/12 PASS**（已将初查时的 692 条 Clippy 报错、68 处非 runtime 未标注 `SAFETY`、43 处 MIR 生产边界违例、`legacy-owners-static` 与 `cargo-fmt` 全部清零）：
  - `language-docs` / `v031-roadmap` / `edge-isolation` / `mms-removal-guard` / `v02-plan-structure` / `native-audit-self-test` / `python-core-tests` / `unsafe-safety` / `native-mir-production` / `legacy-owners-static` / `cargo-fmt` / `cargo-clippy` 全部 `[PASS]`。
- **Rust 功能迁移测试基线（`env -u RUST_MIN_STACK` 默认 2MB 栈，零栈溢出）**：
  - 静态/工具门禁已全绿，但 `codex/0.1.11-closure-to-0.2-dev` 上仍存在 `0.1.11 -> 0.2` 迁移期未收敛的既有功能测试失败（集中在移除 legacy AST 发射、`std/maps.mimi` 改为 `Result<Record, i32>` 返回、`DynamicAnyUnpack`/Canonical MIR 路由与 Z3 `NotInTrustedSubset`），日常开发可使用 `make gates` + 定向 `--lib` 回归护航：
  - `--lib typecheck::`、`--lib ffi_`、`--lib v1_2_verification`、`--lib codegen::mir::`、`--lib dual_`、`--lib tests::audit_fix_scripts`、`--test legacy_owner_boundary_api` 的当前精准通过/失败计数以最终构建日志与 `make test-quick` / `make test-z3` 输出为准（详见本轮验证记录）。

> 注：除了已纳入 `make test-scripts` 的 15 组离线核心 Python 测试文件（179 tests）外，另有 5 组与 WIP schema/SHA-256 锚定绑定较紧的 Python 单测暂未纳入 `run-gates.sh` 默认门禁（`scripts/test_consume_actor_direct_contract_supplements.py`、`scripts/test_merge_actor_census_full_independent.py`、`tests/scripts/test_actor_two_fixture_runner_contract.py`、`tests/scripts/test_route_census_schema_v5.py`，以及 `tests/scripts/test_full_route_census.py` 中需同步调整 `scripts/full_route_census.py` 的部分，后者受 `verify-actor-fallback-direct-api-l1.sh` SHA-256 锚定），待后续 Actor/Census 切片收口后一并纳入。

---

## 4. 下一步仓库清理与结构重构路线图

### 4.1 Git 与工作区整理检查单（当前分支收敛后再执行）

当前工作区有较多并行历史状态：13 个本地分支（含 3 个 `backup/*`、8 个 `codex/*`）、5 个 stash、数百项修改与未跟踪文件。建议按以下安全顺序清理，**不要直接跨过现有未提交修改切换分支**：

1. **切片提交收敛 WIP**：先将工具链/门禁清理变更与 `src/` 中不同语义主题（Map/Any v2、Actor/MIR routing、verifier/FFI）拆分或收敛提交；格式化（`make fmt`）放在独立提交中避免掩盖语义 diff。
2. **清理已合入分支与 Stash**：逐一确认 `git cherry -v codex/0.1.11-closure-to-0.2-dev <branch>` 与 5 个 `git stash list` 的内容已无额外价值后再删除。
3. **根目录与 `docs/` 收尾**：评估是否删除历史快照 [`gramma/mimi-grammar-v1.0.0-rc.md`](../gramma/mimi-grammar-v1.0.0-rc.md)、本地遗留 `AGENTS_ANCHOR_2026-07-14.md`，以及重新刷新 [`docs/language-support.toml`](../docs/language-support.toml) 版本号。

### 4.2 高杠杆结构重构方向（降低编译与认知复杂度）

目前单包 `mimi` 统计包含 **`src/` 567 个文件 / 1,204,993 行**，外加 **`tests/` 根目录 81 个集成测试 crate / 81,749 行**（仅 `tests/real_world_cli.rs` 即达 28,493 行；`src/core/mir/route.rs` 为 60,375 行、`src/verifier/mir.rs` 为 47,160 行、`src/interp/bytecode/mir.rs` 为 44,581 行、`src/core/cfg/resource_lower.rs` 为 41,511 行、`src/core/mir/reference.rs` 为 39,605 行、`src/core/mir/lower.rs` 为 32,857 行、`src/runtime/mod.rs` 为 28,265 行、`src/codegen/mir/ordinary_map_owned_any_v2.rs` 为 25,865 行）。建议分四步拆分：

1. **先拆 Cargo Workspace Crate 边界（优先级最高）**：将编译器拆为 `mimi-span`/`mimi-ast` → `mimi-core`（typecheck/resolved/CFG/MIR）→ `mimi-runtime` → `mimi-codegen` / `mimi-interp` / `mimi-verifier` → `mimi-cli`/`mimi-lsp`。修改测试或 CLI 时无需重编 `runtime`/`verifier`/`codegen` 全部模块，增量 `clippy`/`test` 将显著加速。
2. **合并集成测试入口，减少 81 个独立链接目标**：将大量同族 `tests/canonical_mir_ordinary_map_*.rs` 合并为少数按族组织的多模块集成测试 crate（如 `tests/canonical_mir_map_suite/main.rs`），把大批量 L3 memcheck / bash runner 脚本调用归拢到统一 harness crate 中。
3. **拆解特大单文件与生成式契约模板**：优先为 `src/core/mir/{route,reference,lower,receipt}.rs` 与 `src/codegen/mir/ordinary_map_owned_any_v2.rs` 按阶段/家族建立子模块与参数化数据表，减少“同一规则在 5 个 3–6 万行文件里 copy-paste”的耦合，顺便把 `src/tests/*.rs` 中的重型测试迁出核心库 crate（如 `canonical_scalar_ffi.rs` 31.6k 行、`dual_backend.rs` 24.1k 行）。
4. **收拢 `scripts/` 工具链入口（当前 35 个 `.py` + 71 个 `.sh`，含 50 个 `verify-*.sh`）**：维持 `scripts/run-gates.sh` 作为唯一调度顶层，把零散的 L3 verifier wrapper 脚本逐步收敛到参数化 runner（例如按 fixture manifest 驱动），并让全量 `scripts/` + `tests/scripts/` Python 单测回归单一 `unittest discover` 一键全绿。
