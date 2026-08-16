# mimi-taskq

0.1.7 dogfood: task pipeline with **Flow + Session + contracts + linearity**.

## Coverage

| Feature | Where |
|---------|-------|
| Flow | `TaskFlow`: `Pending -> Running -> Completed/Failed` |
| Session | `session Handoff = !i32 . ?i32 . !i32 . end` typed linear handoff |
| Contracts | `requires` / `ensures` on `next_task_id`, `enqueue`, `priority_score` |
| Linear | each `SessionChan` endpoint consumed in exact protocol order and closed once |

## Run

```bash
mimi run src/main.mimi
mimi test src/main.mimi
mimi build src/main.mimi -o taskq && ./taskq
```

## Acceptance notes

- `mimi check`, native `mimi build`, `mimi run`, and `mimi test` all pass.
- This project is deliberately small enough to read in one sitting but uses
  multiple 0.1.7 high-value features in one program.
