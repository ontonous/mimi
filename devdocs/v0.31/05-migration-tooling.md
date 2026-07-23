# v0.31 语言迁移与工具

## 版本

- **0.31.20**：错误代数、func/fn、签名合约、pure comptime、attribute fail-closed、removed syntax 收敛。
- **0.31.34–36**（deferred to post-1.0）：~~自举可行性 spike / MimiSpec parser 自举 / HM 自举闭环~~。不阻塞 0.1.1。
- **0.31.37**：`mimi migrate --from pre-1.0` 与 fix-it，仓库/stdlib 一次迁移。
- **0.31.38**：token/CST formatter、LSP CheckedProgram/Origin、自动 support probes。
- **0.31.39**：multi-target、dynamic Protocol、Effect/Capability、raw extern experimental 隔离。

## 自举推迟理由

> 自举不产生用户价值。没有用户因为"编译器是自举的"而选择语言。
> 自举的调试地狱（Mimi 编译器 bug 导致 Mimi 写的 parser 报错，无法区分前端逻辑错误还是后端 codegen bug）
> 在语言尚未完全冻结时风险极高。MCDD（真实程序驱动）已更 cheap 地提供语言稳定性验证。
> 推迟到 post-1.0，语言完全稳定后再考虑。

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
