# Phase E 判死/出清状态（0.1.7）

> 来源：`devdocs/v0.37/feature-design-review-0.37.md` §3/§5。
> 本文件是受控文档快照，随删除提交持续更新。

| # | 项 | 状态 | 证据 |
|---|----|------|------|
| 1 | `quote` / `quote!` / `$(...)` 语法面 | **已裁决删除，parser 兼容中** | `docs/syntax-reference.md` §5.3 判死注记；`feature-design-review-0.37.md` #1 |
| 2 | `protocol` 声明 / `impl P` 表面语法 | **已裁决删除，parser 兼容中** | `docs/syntax-reference.md` §6 判死注记；`feature-design-review-0.37.md` #2 |
| 3 | Effect lattice 残余 | `with` 已废；无残留文档误导 | `docs/syntax-reference.md` §6 既有注记 |
| 4 | `ffi slice` / `ffi slice_mut` / `ffi buffer` | **已从 spec §7.4 移除** | `docs/language-spec.md` §7.4 REMOVED 标记 |
| 5 | MTE / 影子内存 | 无 spec 条目，仅 devdocs 愿景 | 全仓 `docs/` 无 MTE 泄漏项 |
| 6 | Component IR registry 补齐 | **Map→JSON 运行时导出已注册** | `src/component/gen.rs` `mimi_map_to_json_*`；dogfood 无 registry 警告 |

## 收尾门槛

- [ ] quote 删除提交（移除 parser 产生式、关键字表、负/正测试迁移）
- [ ] protocol 删除提交（移除顶层语法、checker 投影残留、文档迁移）
- [ ] 最终复核 effect/MTE 相关文档无死链
