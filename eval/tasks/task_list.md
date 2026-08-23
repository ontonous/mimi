# Phase F 任务集（0.39.111 起）

| ID | 类别 | 要求 | 正例 | 期望输出 |
|---|---|---|---|---|
| t01 | Flow | 定义带字段的 Flow，Pending→Shipped transition | `t01_flow.mimi` | `t01_flow ok` |
| t02 | 线性 | `linear T` 直通 cap；make_token→token_id 恰一次（输出不依赖进程级计数器） | `t02_linear.mimi` | `t02_linear ok` |
| t03 | Session | typed session_open，send 后 close（线性协议） | `t03_session.mimi` | `t03_session ok` |
| t04 | actor runs Flow | actor 绑定 Flow 并可方法化 | `t04_actor_flow.mimi` | `t04_actor_flow ok` |
| t05 | 失败分层 | Result/Option 函数边界 + fails E 回滚 | `t05_failure.mimi` | `5` |
| t06 | 对照 CRUD | 记录 + 集合读写 | `t06_crud.mimi` | `2\na\ntrue` |

## 评测口径
- 候选程序只给**任务描述**（对应列「要求」），不展示正例。
- 判定：check 过 → `mimi run` 输出 == 期望输出 → `mimi build` 输出一致。
- 逃生舱滥用：出现 `cap` 声明 / `mms{}` / thread_local cap 协议 = 该题记滥用。
- 冻结：`devdocs/mimi-eval/freeze.toml`（编译器版本、内核卡 SHA、模型、采样、
  最大修复轮次）。
