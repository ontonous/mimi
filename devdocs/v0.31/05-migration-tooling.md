# v0.31 语言迁移与工具

## 版本

- **0.31.16**：错误代数、func/fn、签名合约、pure comptime、attribute fail-closed、removed syntax 收敛。
- **0.31.30**：`mimi migrate --from pre-1.0` 与 fix-it，仓库/stdlib 一次迁移。
- **0.31.31**：token/CST formatter、LSP CheckedProgram/Origin、自动 support probes。
- **0.31.32**：multi-target、dynamic Protocol、Effect/Capability、raw extern experimental 隔离。

## 迁移原则

1. 目标语义先在 Resolved IR 和双后端完成。
2. 提供 diagnostic/fix-it。
3. 一次迁移 repo、stdlib、real_world。
4. 删除 stable parser/checker/backend 旧路径。
5. 兼容只存在于独立 migrate 命令，主编译器不维护双语法。

## 门禁

- formatter round-trip 保持 CST/AST/semantics。
- LSP hover/completion 显示 qualified state、generation、permission、Session residual。
- 迁移成功率 100%，二次运行幂等。
- support complete 必须由 probe 产生，禁止人工提升。
