# Canonical Semantic Trace 1

> Normative profile: `mimi-semantic-trace-1`
> Binding source: `docs/language-requirements.toml`. Only requirements whose `profile` contains `mimi-semantic-trace-1` are bound.

## 1. 事件结构

```text
TraceEvent {
  trace_id, event_id, parent_event_id,
  logical_actor, logical_clock,
  kind, requirement_id,
  flow_instance, generation_before, generation_after,
  state_before, event, transition, state_after,
  result_or_fault,
  resource_actions,
  transaction_actions,
  session_before, session_after,
  boundary_call_id, external_identity, external_revision,
  source_span,
}
```

地址、线程 ID、墙钟、allocator 地址和随机 hash 不进入比较键。类型、状态、variant 和 transition 使用 canonical nominal ID。

## 2. 规范化

- map/set 字段按 canonical key 排序；
- opaque handle 替换为 trace 内首次出现顺序的 logical ID；
- source path 相对 package root；
- suppressed/secondary failure 保持因果顺序；
- payload 只记录规范声明可观察的字段和稳定摘要。

## 3. 等价

确定性程序要求事件序列逐项相等。并发程序构造 happens-before DAG：

- 同一 Actor turn 顺序；
- spawn 在 child 首事件之前；
- send 在对应 receive 之前；
- Session residual 操作按 endpoint 顺序；
- resource acquire 在 move/drop 之前；
- boundary request 在 terminal callback/cancel 之前。

两个 trace 等价，当且仅当节点标签多重集相同且存在保持 happens-before、logical identity 和 terminal outcome 的图同构。无依赖并发事件允许重排。

## 4. 门禁

L1 不只比较 stdout，还比较 Flow/Fault/resource/transaction/Session/Actor/boundary trace。任何后端遗漏事件、使用不同 typed outcome 或产生额外业务边均失败。
