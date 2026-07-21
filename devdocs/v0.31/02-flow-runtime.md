# v0.31 Flow、Actor、Session 与资源

## 版本

- **0.31.8**：nominal Flow identity、linear generation、state unforgeability。
- **0.31.9**：draft + Become/Stay/Fault/Rejected 原子 turn，transition `?` 归还 source。
- **0.31.10**：稀疏图、per-Flow typed Fault、显式 reset/recover、progressive Main 真 lowering。
- **0.31.11**：Actor runs Flow，移除稳定 Actor mutable business field。
- **0.31.12**：typed dual Session residual，覆盖 alias/field/return/branch/close。
- **0.31.13**：transition/Fault/Session resource exactly-once 与闭包 ownership。
- **0.31.14**：static Protocol stable；dynamic VTable 保持 experimental。
- **0.31.15**：canonical semantic trace 和 happens-before DAG comparator。
- **0.31.16**：攻击审查 I，0 新 feature。

## 核心门禁

- 旧 generation 使用、状态伪造、未声明业务边全部静态拒绝。
- 四 terminal outcome 的 interp/native trace 一致。
- 非 end endpoint 不得静默离开 scope。
- Fault/kill/timeout 后资源 exactly-once，L3 零 UAF/double free/leak。
- Actor/Session 并发 trace 采用偏序等价，不比较墙钟顺序。
