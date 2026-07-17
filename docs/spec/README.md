# Mimi 1.0 Normative Appendices

本目录细化 `docs/language-spec.md` 的稳定语义。主规范定义用户合同；本目录定义实现该合同所需的数据模型、状态转移、编码和比较算法。附录不得自行提升 feature 稳定性。

| 附录 | 规范内容 |
|---|---|
| `resolved-ir.md` | checker 输出、稳定 ID、backend capability gate |
| `transition-turn.md` | Flow generation、draft、四类 terminal outcome、effect 可见点 |
| `semantic-trace.md` | MCDD canonical trace 和并发偏序等价 |
| `verified-core-1.md` | Trusted subset、Verification IR、证明结果与 artifact |
| `native-abi-1.md` | 同进程布局、handle、allocator、callback、async 生命周期 |
| `wire-schema-1.md` | 跨进程 canonical envelope、版本、限制和冲突协议 |

所有附录版本独立冻结。破坏机器语义、ABI 或 wire compatibility 的修改必须提升对应 major semantics/profile 版本。
