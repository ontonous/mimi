# mimi-ledger

0.1.7 dogfood: account lifecycle with **Flow + Session + contracts + linearity**.

## Coverage

| Feature | Where |
|---------|-------|
| Flow | `Account`: `Active -> Frozen -> Active` lifecycle |
| Session | `session Audit = !i32 . ?i32 . end` one-shot audit channel |
| Contracts | balance/deposit/withdraw `requires` / `ensures` |
| Linear | audit `SessionChan` endpoints moved through send/recv exactly once |

## Run

```bash
mimi run src/main.mimi
mimi test src/main.mimi
mimi build src/main.mimi -o ledger && ./ledger
```

## Acceptance notes

- Exercises flow state payload rebuilds, typed dual sessions, and runtime
  contract checks in the same native binary.
