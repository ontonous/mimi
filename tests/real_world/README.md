# Mimi 真实代码可用性测试套件（MCDD）

本目录按 [AGENTS.md §13.13 MCDD](../../AGENTS.md) 的方法，用完整的 `.mimi` 程序检查 Mimi 语言各特性在解释器与 LLVM codegen 双后端下的可用性。

## 目录结构

```
tests/real_world/
├── generate_suite.py      # 生成所有 .mimi 测试文件
├── run_suite.py           # 批量跑 mimi run + mimi build + 执行二进制
├── README.md              # 本文件
├── RESULTS.md             # 最新评估结果
├── *.mimi                 # 按特性/标准库模块组织的真实程序
└── projects/              # 包导入测试用的本地项目
    ├── mylib/             # 被依赖的库（pub func）
    └── consumer/          # 使用 use mylib::func / use mylib 的 consumer
```

## 使用方法

```bash
# 1. 确保 release 版 mimi CLI 已构建
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo build --release

# 2. 重新生成测试文件（修改 generate_suite.py 后执行）
cd tests/real_world
python3 generate_suite.py

# 3. 运行完整套件
python3 run_suite.py
```

`run_suite.py` 会为每个 `.mimi` 文件执行：
- `mimi run <file>`（解释器路径）
- `mimi build <file>`（codegen 路径）
- 运行生成的可执行文件（若 build 成功）

`main()` 返回 `0` 表示该测试通过，非零表示失败。

## 设计原则

1. **真实代码**：每个文件都是可独立运行的完整程序，不是最小片段。
2. **双后端等价**：同一份源码同时跑解释器和 codegen，暴露后端差异。
3. **退出码断言**：通过 `main` 返回值判断，便于脚本批量处理。
4. **覆盖核心 + stdlib**：语言核心、并发原语、Actor、标准库、包导入等。

## 与 cargo test 的关系

本目录由 Python 驱动，用于快速本地评估。已有的 `tests/real_world.rs` 是更严格的 cargo integration test，包含 10 个硬编码端到端用例。两者互补：
- `tests/real_world.rs`：CI 门禁，断言必须全部通过。
- `tests/real_world/*.mimi`：特性矩阵扫描，允许记录已知差距。
