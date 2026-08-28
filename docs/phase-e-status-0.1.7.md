# Phase E 判死/出清状态（0.1.7）

> 来源：`v0.37/feature-design-review-0.37.md` §3/§5。
> 本文件是受控文档快照，随删除提交持续更新。

| # | 项 | 状态 | 证据 |
|---|----|------|------|
| 1 | `quote` / `quote!` / `$(...)` 语法面 | **已移除** | parser 拒绝并给出 0.1.7 Phase E 迁移错误；`comptime` 保留常量折叠；负/正测试迁移完成 |
| 2 | `protocol` 声明 / `impl P` 表面语法 | **已移除** | parser 对顶层 `protocol` 与 Flow 内 `impl P` 给出 Phase E 错误；AST/Resolved/Verifier/Codegen/Interp protocol 目录与投影残留已清除；`flow_protocol.mimi` 语料移除 |
| 3 | Effect lattice 残余 | `with` 已废；无残留文档误导 | `docs/syntax-reference.md` §6 既有注记 |
| 4 | `ffi slice` / `ffi slice_mut` / `ffi buffer` | **已从 spec §7.4 移除** | `docs/language-spec.md` §7.4 REMOVED 标记 |
| 5 | MTE / 影子内存 | 无 spec 条目，仅 devdocs 愿景 | 全仓 `docs/` 无 MTE 泄漏项 |
| 6 | Component IR registry 补齐 | **Map→JSON 运行时导出已注册** | `src/component/gen.rs` `mimi_map_to_json_*`；dogfood 无 registry 警告 |
| 7 | Wire / ABI CLI 接入 | **已接入** | `mimi wire encode|decode|validate-schema` 与 `mimi abi core|export|validate|hash|diff|check|emit-c|emit-rust` 已落地并带 CLI 单测 |

## 收尾门槛

- [x] quote 删除提交（移除 parser 产生式、关键字表、负/正测试迁移）
- [x] protocol 删除提交（移除顶层语法、checker 投影残留、文档迁移）
- [x] 最终复核 effect/MTE 相关文档无死链
  - `docs/` 下所有 Markdown 本地链接扫描通过；`effect` 文档仅保留 spec/capability 语义，`MTE/影子内存` 无 spec 泄漏项（2026-08 Phase E final check）
