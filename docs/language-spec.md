# Mimi Language Specification (1.0 Draft)

> **Authority**: This document is the single canonical specification entry point for Mimi 1.0.
> It is extracted from the Pre-0.1 design contracts in `devdocs/pre-0.1/` (00–08).
> All other documentation must defer to this file for semantic definitions.
>
> **Target status**: Normative sections are `stable` unless explicitly marked `experimental`, `reserved`, or `removed`. Current implementation maturity is non-normative and lives only in `docs/language-support.toml`.
>
> **⚠ Philosophy note (2026-08)**: `stable` here means **anchored for the current
> development cycle** (a stable target for sprints), **not a frozen 1.0 API
> commitment**. Mimi is an early experimental language far from 1.0; breaking
> changes are freely allowed pre-1.0, including overturning "frozen" syntax. The
> long-term assets are the design ideas and the invariant suite (L1/L2/L3 +
> dual-backend equivalence), not any particular surface spelling. See
> `devdocs/v0.36/README.md`.
>
> **Version**: v1.0-spec-draft (2026-07-17)

> **⚠ 实现差异登记（2026-08-01 立账，0.34.33 全部闭环）**：本文件为 1.0 规范草案；
> 以下条目曾与 parser/checker 实况矛盾，现**全部裁决并闭环**（保留为历史台账，
> 详细证据见 `devdocs/v0.34/golden-document.md` 与
> `devdocs/v0.34/golden/syntax-reference.golden.md`）：
>
> | 规范位置 | 规范主张 | 实现实况 | 处置 |
> |---------|---------|---------|------|
> | §6.1 `LANG-FUNCTION-001` | `func(T)->U` **removed**，迁移到 `fn(T)->U` | `func(T)->U` 仍解析（parse_type.rs:213-237）；`fn` 仅限闭包表达式与 `extern "C" fn(...)` 类型（parse_type.rs:242） | ✅ ADR-003 裁决：保留现状，spec 修正（0.34.4） |
> | §3.12 `FLOW-FAULT-001` | fault 变体块 `fault F { A \| B }` + `fault Variant(...)` terminal + `reset`/`recover` 语句 | 仅 `fault ErrorType`（top_level.rs:1216-1222）；变体块语法全仓零匹配；reset/recover 仅系统注入 transition 名 | ✅ 0.34.17 收缩到现实：spec §3.12 改由实现驱动（per-flow `fault ErrorType` + 系统 Fault payload 文档化 + recover=业务转移）；变体块 fault-set 排 0.2（golden §3.2） |
> | §6.12 `SYNTAX-REMOVED-001` | `\|>` 已 removed | parser 仍接受 `transition t(A) -> X \|> Y`（top_level.rs:1349-1354，`\|>` 与 `\|` 都接受） | ✅ 0.34.1 删除：`\|>` 现为专用拒绝诊断（top_level.rs:1357-1368），`-> A \| B` 为唯一多目标分隔符（golden §1.1） |
> | §7.9 | `stay { payload }` 带 payload 形式 | 仅裸 `stay;`（parse_stmt.rs:134-137） | ✅ ADR-001 实施（0.34.11）：`become`/`stay` 均删除，唯一终止符 `return S{}`（golden §1.2） |

Normative requirements use stable IDs defined in `docs/language-requirements.toml`. Design rationale lives in `devdocs/pre-0.1/`; implementation structure and progress live in `docs/ast-appendix.md` and `docs/language-support.toml`. Parser acceptance and an existing implementation do not grant stable status.

Normative implementation profiles are defined in:

- `docs/spec/resolved-ir.md` (`mimi-resolved-ir-1`);
- `docs/spec/transition-turn.md` (`mimi-flow-turn-1`);
- `docs/spec/semantic-trace.md` (`mimi-semantic-trace-1`);
- `docs/spec/verified-core-1.md` (`mimi-verified-core-1`);
- `docs/spec/native-abi-1.md` (`mimi-native-abi-1`);
- `docs/spec/wire-schema-1.md` (`mimi-wire-schema-1`).

These appendices refine this specification and are normative only for the requirements that cite their profile. They cannot promote an experimental feature.

### Normative Requirement Map

| Requirement | Section |
|---|---|
| `FLOW-IDENTITY-001` | §3.1 |
| `FLOW-TURN-001` | §3.4 |
| `FLOW-SPARSE-001` | §3.5 |
| `FLOW-MULTI-001` | §3.7 |
| `ACTOR-FLOW-001` | §3.8 |
| `PROTOCOL-STATIC-001`, `PROTOCOL-DYN-001` | §3.9 |
| `SESSION-LINEAR-001` | §3.10 |
| `RESOURCE-LINEAR-001` | §3.11 |
| `FLOW-FAULT-001` | §3.12 |
| `FLOW-PROGRESSIVE-001` | §3.13 |
| `FLOW-EPOCH-DROP-001` | §6.4 |
| `ERROR-ALGEBRA-001`, `ERROR-PROP-001` | §4, §4.2 |
| `OWN-PERMISSION-001` | §6.2 |
| `EFFECT-CAP-001` | §2.7 |
| `VERIFY-CORE-001` | §5 |
| `COMPTIME-PURE-001` | §6.10 |
| `SYNTAX-REMOVED-001` | §6.12 |
| `LANG-FUNCTION-001` | §6.1 |
| `LANG-CONTRACT-001` | §6.8 |
| `LANG-ATTRIBUTE-001` | §6.11 |
| `COMPONENT-IR-001` | §7.3 |
| `COMPONENT-HANDLE-001` | §7.5 |
| `COMPONENT-CALLBACK-001` | §7.7 |
| `COMPONENT-ASYNC-001` | §7.8 |
| `COMPONENT-WIRE-001` | §7.8.1 |
| `COMPONENT-RAW-001` | §7.2 |
| `TOOL-RESOLUTION-001` | §4.10 |
| `TOOL-SUPPORT-001`, `MIGRATION-PRE1-001` | §9 |
| `MULTILANG-AUTHORITY-001` | §8 |

---

## 1. Language Positioning

*[source: devdocs/pre-0.1/00-core-goals.md §1–§3]*

Mimi is a **Flow-first, Typestate-Oriented** systems programming language.

Its core value is not replicating Rust's borrow syntax, Actor languages' message syntax, or traditional Design by Contract. It uses one composable model to answer five questions:

1. What state is a business object currently in?
2. Which business events are allowed in the current state?
3. How do resources and ownership transfer when state changes?
4. Is a failure a local return, a state fault, or a concurrent peer fault?
5. Which errors can be rejected before the program runs?

Mimi 1.0 must make these answers directly visible from source code and types, not dependent on runtime logs, implicit sentinels, or backend implementation details.

### 1.1 Flow-first, not Flow-everything

Flow-first does not mean every addition function must declare a state machine.

Plain `func` is appropriate for: `[stable]`

- Pure computation;
- Synchronous input-to-output transformation;
- Local mutable work that ends within the call;
- Helper logic that does not retain state across calls.

The following must enter Flow: `[stable]`

- Persisting mutable business state across time;
- Resource phases that span synchronous calls, enter Flow payload, or participate in recovery;
- Actor receiving messages and changing business state;
- Session endpoint advancing communication phases;
- Requiring reset, recover, or supervision strategy after fault;
- Whether an operation is allowed depends on the object's current state;
- Version changes of external facts that change allowed Mimi business behavior.

The judgment criterion is not "is the code complex," but "does state persist across a synchronous call and participate in Mimi's business legality." External library caches, GUI animations, and database internal indices can be owned by their components; Flow only holds the typed reference, revision, and policy that business needs.

### 1.2 Minimum Mental Model

A 1.0 user needs the following core constructs. Entries are `[stable]` unless their row explicitly says otherwise.

| Construct | Unique Responsibility |
|---|---|
| `func` | Stateless synchronous computation and composition |
| `flow` | Business state and its legal changes that persist across time |
| `actor` | Mailbox, scheduling, isolation, and supervision; business state carried by Flow |
| `protocol` | Static state topology visible externally from a Flow |
| `session` | Message ordering between two linear endpoints |
| `Result<T, E>` | Synchronous, recoverable failure |
| `Fault` | State fault where a Flow invariant is broken or cannot continue |
| `PeerFault` | Typed propagation of concurrent peer failure |
| `view / mutate / consume` | Read-only, in-place modification, and ownership transfer permissions |
| `effect / capability` `[experimental]` | Describes what an operation may do, and whether the caller is authorized |
| `requires / ensures / invariant` | Contracts that can be dynamically checked or statically proven in a trusted subset |
| `component / foreign` | Brings external language capabilities into Mimi's type, state, and fault model |

Different constructs must not compete for the same responsibility. For example, `Fault` is not an alternative spelling of `Result::Err`; Actor fields are not a second business state model bypassing Flow.

---

## 2. Design Invariants

*[source: devdocs/pre-0.1/00-core-goals.md §4]*

### 2.1 State Invariants `[stable]`

- Flow states have fully qualified nominal identity, e.g., `Order::Paid`.
- State values cannot be arbitrarily forged from outside the Flow.
- A Flow instance has exactly one current state at any moment.
- Transition consumes the old state; the old state cannot be used after transition.
- Self-loops also produce a new state generation; old aliases cannot be retained.
- Multi-target transitions must preserve runtime state tag. `[stable]`
- Each transition turn must end with exactly one of: `return Target { ... }`, typed `fault`, or rollback failure.
- State commit is atomic; failure must not leave a half-updated payload.

### 2.2 Compilation Invariants `[stable]`

- Programs accepted by the checker must be correctly implementable by all committed backends.
- Unsupported stable semantics must error at check time, not deferred to codegen.
- Codegen must only consume the unique semantics resolved by the checker; it must not re-guess transitions, types, or targets.
- Unknown attributes, annotations, states, events, and capabilities must fail-closed.
- Warnings must not substitute for hard errors required for correctness.

### 2.3 Failure Invariants `[stable]`

- Synchronous failure, Flow Fault, PeerFault, panic, and process exit must be different channels.
- `?` propagates only based on static `Result`/`Option` type; leaving a normal function returns, entering a transition's declared rollback failure path and returning source generation.
- Flow `fails E` is a rollback channel: the source generation is returned to the caller with a typed error. Flow `Fault` is a system state: the flow has moved to a recoverable failure state. Diagnostics must describe them as separate channels and must not refer to `Fault` as a second `Result::Err`.
- Runtime/FFI must not use the same `0`, `null`, or `-1` to represent multiple failures.
- Recover must be a business-defined state transition, not default-value construction of fake external resources.
- Dynamic untrusted input must first be decoded into typed events; failure returns typed boundary error.

### 2.4 Ownership Invariants `[stable]`

- `view` does not allow modification or ownership transfer.
- `mutate` allows constrained in-place modification; no undeclared reallocation or ownership escape.
- `consume` or by-value parameter transfers ownership.
- Each linear resource must be transferred, returned, or dropped exactly once on all control flow paths.
- `shared`/`weak` are explicit shared object graphs; must not be silently treated as bare values.
- Local resources existing only within a synchronous call are managed by lexical ownership and scope guards, not forced into Flow state.

### 2.5 Verification Invariants `[stable]`

- Only programs entering the versioned trusted sublanguage can receive `Proven`.
- Solver unavailable, timeout, Unknown, unsupported, and crash must not masquerade as proof success.
- Static proof must correspond to interpreter and native backend machine semantics.
- Z3 is not responsible for proving Flow, Actor, or Session unless there is a clear dedicated logic and versioned model in the future.

### 2.6 Cross-language Invariants `[stable]`

- Component Boundary is a first-class citizen of Mimi; in-process FFI is one transport, not an escape hatch bypassing the type system.
- External components must not directly own or modify Mimi Flow payload.
- Each cross-language boundary must declare ownership, error, effect, thread, callback, async, and version semantics.
- Flow, Protocol, Session, Fault, and capability retain type identity across boundaries; must not degrade to bare integers or `void*`.
- GUI can only submit commands and consume immutable snapshot/event; cannot become a second authority for business state.
- All language bindings must be generated from the same Component IR.
- Untrusted or potentially stuck external components must support process isolation and typed `ForeignFault`.
- In-process ABI, IPC, WebSocket, WASM, and worker process must project the same Component/Protocol semantics.

### 2.7 Capability Invariants

> **0.34.18c (§4.2 ruling): the `with` effect clause is abolished.** Function effect
> annotations (`func f() with io`) were a parseable-but-unenforced model that
> guaranteed nothing and duplicated contracts; the parser now rejects `with`
> (reserved keyword). Side-effect obligations are expressed by contracts
> (`requires`/`ensures`) and capability tokens. The Effect half of this section is
> therefore removed; the Capability invariants below remain (the `cap` token is a
> core linear carrier, enforced by E0256).

- Capability describes whether the caller is authorized to trigger an operation.
- Capability is nominal, unforgeable, scope/audience-restrictable, revocable; holding a capability does not change the Flow's currently allowed event set.
- Capability tokens are linear: a `cap` value must be consumed exactly once (E0256); issuance/delegation/revocation semantics are part of the linear-capability model.

The capability surface remains experimental until issuance/delegation/revocation semantics and resolved summaries are frozen. Mutation safety is carried by the `view/mutate/consume` permission model (§3.3), not by effects.

---

## 3. Flow-first Core Model

*[source: devdocs/pre-0.1/01-flow-first-model.md §2–§12]*

### 3.1 Flow Instance and Linear Identity `[stable]`

Conceptually, each Flow instance carries:

```text
FlowInstance<FlowId, StateId, Generation, Payload>
```

Users do not write this internal type, but the language must guarantee:

- `FlowId` distinguishes different Flows;
- `StateId` is a fully qualified nominal state;
- `Generation` prevents handles before and after transition from being simultaneously valid;
- `Payload` belongs only to the current state;
- Flow instances are non-copyable by default;
- Transition consumes input instances and produces the next generation.

### 3.2 State Unforgeability `[stable]`
External code cannot arbitrarily construct state payloads. Stable API should produce states only via Flow constructor, transition, or controlled recovery entry.

Within a Flow, short state names can be used; outside, fully qualified names or opaque handles must be used. Two states both named `Active` in different Flows are never the same type, even with identical field layout.

### 3.3 Transition is the Only State Change `[stable]`

All cross-time observable state changes must go through transition:

```text
transition : Flow@Source × EventArgs -> Flow@Target
```

The stable checker must reject:

- Calling from wrong Source state;
- Using old Source after the call;
- Bypassing state change through field writes;
- Overloads that codegen cannot uniquely resolve;
- Degrading state handles to integers or untyped pointers.

Codegen must not select the first candidate transition on resolution failure. The checker should output a unique resolved transition ID, consumed by both interpreter and native backend.

### 3.4 Transition Turn and Atomic Commit `[stable]`

Each transition is an exclusive, auditable state turn:

```text
acquire source generation
  -> prepare local draft and effects
  -> exactly one terminal action
  -> atomically publish target generation
```

Terminal actions are exactly three kinds:

- `return Target { ... }`: commit new state (the target may be the system `Fault` state — see §3.12);
- enter `Fault`: commit the system Fault state via an undefined-event auto-fallback or an absorbed runtime panic (§3.12);
- Rollback failure: no new state committed; caller regains original source generation and typed error.

> v0.34.11 (ADR-001): `return State { ... }` is the **unique** transition
> terminal. `become`/`stay` were removed — a self-loop is written as an
> explicit `return SourceState { ... }`. Entering `Fault` explicitly is written
> as `return Fault { ... }` (Fault is a reservable target state); the `?`
> operator takes the separate rollback path, not the Fault path.
>
> v0.1.8 Phase F (move-rest): a transition may keep most fields unchanged with
> `return Target { f: new_expr, ..self }`. The explicit fields are evaluated
> first; `..self` then moves every field not explicitly named into the new
> state and consumes `self`. Copying an explicitly named field back from
> `self` in the same record update is rejected (linear fields must be moved,
> not copied). This is a move of the remaining payload, not a silent copy.

Rollback failure conceptually is `Rejected { source: Flow@Source, error: E }`. Transition signature must declare `E`; `?` in body can only enter this path. It does not implicitly enter Fault, exit the process, or discard Source.

Transition body cannot publish partial state through ordinary field writes. Payload modification first occurs in a private draft; only the terminal action makes it visible.

Irreversible external effects must not be hidden in a rollback turn. They must:

- Split into "initiate" and "complete" transitions via ForeignTask/Actor event; or
- Declare idempotent key and compensation; or
- Enter an explicitly non-rollback business state.

### 3.5 Sparse Business Transition Graph `[stable]`

Flow is a sparse, closed, typed business graph, not a Cartesian product table of states and events.

Users write only business edges:

```mimi
flow Order {
    state Pending
    state Paid
    state Shipped
    state Cancelled

    transition pay(Pending, payment: Payment) -> Paid { ... }
    transition cancel(Pending, reason: Reason) -> Cancelled { ... }
    transition ship(Paid, tracking: Tracking) -> Shipped { ... }
}
```

This Flow has three business edges. It does not have `pay(Paid)`, `ship(Pending)`, or `cancel(Shipped)`.

#### Undeclared Combinations

Undeclared `(state, event)` does not generate an implicit business transition.

- When static state is known, that call does not exist in the type system and must fail compilation.
- The compile-time rejection should list the events that are legal from the actual state (sparse DX), e.g. `legal events from Open: close, peer_fault`.
- Type-erased or dynamic dispatch must carry verifiable Protocol/VTable metadata.
- Network, FFI, deserialization, and other untrusted input must first decode and validate state at the boundary.
- Dynamic validation failure produces typed `UnexpectedEvent`, not a fake business edge to Fault.

#### Automatic Matrix Completion

Stable mode prohibits auto-completing undeclared combinations into business transitions. `[experimental]`

Automatic boundary fallback is only permitted for:

- prototype/REPL;
- Explicitly declared open dynamic boundaries;
- Test fault injection.

Even in prototype mode, the compiler should report implicitly generated locations and event sets.

### 3.6 Event Model `[stable]`

Events are typed inputs to transitions. Users are not required to repeat a global event matrix for each state.

Event sets can be derived from transition signatures:

```text
AllowedEvents<Pending> = Pay(Payment) | Cancel(Reason)
AllowedEvents<Paid> = Ship(Tracking)
AllowedEvents<Shipped> = Never
```

For external dynamic events, encode as tagged value:

```mimi
type OrderEvent {
    Pay(Payment)
    Cancel(Reason)
    Ship(Tracking)
}
```

A dispatcher does one boundary check at the current state, then calls the resolved transition. Dynamic check is not adding new edges to the business graph.

### 3.7 Multi-target Transition `[stable]`

Multi-target transition is **stable** (implemented 0.34.15-16, ADR-002, tagged-union ABI;
19 门禁测试绿，含 multi_target_codegen_dual_backend_tag_dispatch). Requirements:

```mimi
transition decide(Pending) -> Approved | Rejected { ... }
```

Requirements:

- Return value is a closed tagged state union;
- Caller must match or have state refined by control flow before continuing;
- Identical payload layout cannot substitute for state tag;
- Interpreter and native backend produce the same tag;
- Nominal return type must not steal the first target.

Implementations not meeting these requirements must not accept multi-target transitions.

> v0.34.28: superseded — multi-target shipped stable in 0.34.15-16; the old
> "not part of the minimum 1.0 RC stable core" clause is rescinded (golden §3.4,
> ADR-002). Typed dynamic boundary errors remain independent.

### 3.8 Actor and Flow `[stable]`

#### Responsibility Separation

- Flow: business state, events, and transitions.
- Actor: mailbox, scheduling, isolation, quota, supervision, and lifecycle.

Actor no longer owns a second set of arbitrarily modifiable business field model. Stable Actor's business payload must be carried by its Flow.

Target semantics:

```mimi
actor OrderWorker runs Order {
    mailbox depth = 128
    children max = 8
}
```

Final syntax is determined by formal grammar design, but semantics must satisfy:

- Actor runtime internally holds a unique Flow instance;
- Mailbox message decodes to Flow event;
- Each actor turn atomically executes one transition;
- State-relevant calls initiated within the current turn are statically limited by checker;
- External async senders can only statically guarantee messages belong to public Protocol; cannot pretend to know the state when the message arrives;
- Messages can carry expected generation/revision; on mismatch, return typed `StaleGeneration` or `UnexpectedEvent`;
- Dynamic external message failure is typed boundary error;
- Ordinary Actor helpers can only perform stateless computation.

#### `mut` Field Semantics (SD-5 废止于 0.1.8 Phase D)

0.1.8 Phase D 废止 SD-5 逃生舱：用户可见的业务 `mut` 字段一律非法
（E0402，见 §3.8 与 checker `items.rs`）。Actor 不再持有第二套可任意修改的
业务字段模型；其业务载荷必须由 Flow 承载。

- 任何 `actor Name { mut field: T }` 都被 E0402 拒绝，并给出「移除 `mut` 或改写为
  `flow Name { state Ready { field: T } ... }` + `actor Name runs Name`」的改写提示；
- `actor Name runs FlowName` 同样拒绝 `mut` 业务字段：状态由 Flow 携带，可变字段
  破坏原子 turn 保证；
- 业务可写状态只有一条通道：Flow 状态转移（state transition as the only
  state-change channel）；
- 非业务字段（per-instance 元数据、运行时内部状态）可在不写 `mut` 的形态下存在，
  但不得作为业务状态模型。

> 旧文档称 `mut` 为「并发隔离声明标记 / 简单状态逃生舱」——该叙述属于被废止的
> SD-5，0.1.8 起不再成立。

#### Lifecycle

parent/child, detached, PeerFault, SystemKill, and backpressure must share the same state model across all execution backends.

Actor call failure must not return indistinguishable `0`. Call results distinguish at least:

- Success payload;
- Actor has Faulted;
- Actor has terminated;
- Mailbox full or timeout;
- Unknown/event not allowed in current state;
- Peer/system kill;
- Runtime infrastructure failure.

### 3.9 Protocol surface `[removed 0.1.7 Phase E]`

`protocol` 声明 / `impl ProtocolName` 表面语法已在 0.1.7 Phase E 从 parser
移除（`feature-design-review-0.37.md` #2）。原静态拓扑投影由 Flow 自身的
state/transition 声明直接承载；checker-only 静态投影与 `dyn Protocol` 均交给
宿主语言。保留的组件边界 Protocol 概念仅用于外部 ABI/schema 语义，不再是
Mimi 语言内可写表面。

### 3.10 Session `[stable]` / `[experimental]`

Session describes communication ordering between two linear endpoints.

Conceptual API:

```mimi
let (client, server): (
    SessionChan<ClientProtocol>,
    SessionChan<dual ClientProtocol>
) = session_pair::<ClientProtocol>()
```

Each operation advances residual:

```mimi
let client1 = send(client, request)
let (reply, client2) = recv(client1)
close(client2)
```

**`[stable]` — checker-level linearity (implemented):**

- Old endpoint invalid after operation;
- Endpoint must not implicitly convert to integer;
- Alias, fields, return values, and branch merge preserve residual;
- Cannot skip check when unable to track endpoint; must error;
- Non-`end` endpoint leaving scope must explicitly return, transfer, or error;
- Session runtime and checker use the same protocol ID;
- Typed residual diagnostics: scope-exit residual (E0425), use-after-alias (E0426), double-close (E0304), protocol-conformance × linearity / payload downgrade (E0427);
- Endpoint as function argument consumes residual; branch partial-consume rejected.

Any Session program that cannot prove residual completeness must be rejected.

**`[experimental]` — not yet closed:**

- cross-turn exactly-once and Fault-path resource cleanup;
- recursive protocols, dynamic participants, delegation, multiparty Session, and cross-version residual upgrade.

Minimum dual-end linear Session is a 1.0 core goal; any unclosed item blocks RC. Codegen residual lowering is now covered by dual-backend regression for roundtrip, branch-merge, loop, typed-pair, and match-arm residual forms (0.36.19 / 0.36.38 / 0.36.41); the remaining experimental items are listed above.

### 3.11 Resources and State `[stable]`

Phased resources should be expressed through typestate or Flow payload, e.g., socket's Unbound/Connected/Closed.

Local resources created and released within a single synchronous call need not become Flow state; they are managed by linear local variables and `defer` (scope guard). A resource enters Flow payload only when it survives across turns, changes allowed operations, participates in Actor/Session, or needs recovery.

Leaving a state, the checker must prove each linear resource on all paths:

- Moved into target state;
- Returned to caller;
- Transferred to child/session;
- Or exactly-once drop.

Fault, reset, and recover must not use `unit`, empty list, or zero value to substitute for external resources that cannot be default-constructed. Resource recovery must be defined by explicit business transition.

`persistent` only indicates cross-Fault ownership retention strategy; it does not automatically prove data consistency. `transactional` must provide the same commit and rollback semantics in all execution backends.

### 3.12 Fault and Recovery `[stable]`

A Flow that cannot maintain its business invariants enters a system-injected
`Fault` state. Fault is per-Flow and typed; it is not a global catch-all record,
and a program may not declare a state named `Fault` (the name is reserved for the
system sink).

**Declaring a typed fault.** A flow may declare a single typed error with
`fault ErrorType` in its body. When present, the injected `Fault` state carries an
additional `error: ErrorType` field alongside the system payload:

```mimi
type AccountError {
    code: i32,
    reason: string,
}

flow Account {
    state Active { balance: i32 }
    fault AccountError

    transition deposit(Active, amount: i32) -> Active {
        return Active { balance: self.balance + amount }
    }
}
```

**The Fault payload.** Every `Fault` value carries the following system fields
(formalized here; previously implemented but undocumented):

| Field | Type | Meaning |
|-------|------|---------|
| `last_state` | `flow::<F>::StateId` | Nominal identity of the state the flow was in when the fault occurred. |
| `unexpected_event` | `flow::<F>::EventId` | Nominal identity of the unhandled event; an absorbed runtime panic is `Panic { code: ... }`. |
| `snapshot` | `string` | A textual snapshot of the faulting state. |
| `trace` | `SystemTrace` | Structured diagnostics. Its `last_state_name` and `unexpected_event` are human-readable strings; state identity remains in the nominal fields above. |
| `error` | `ErrorType` | Present only when `fault ErrorType` is declared; the per-flow typed error. |

**Entering Fault.** A flow enters `Fault` through two channels:

1. *Undefined event (auto-fallback).* Calling a declared event from a state that
   has no user-defined transition for it returns a `Fault` value whose
   `last_state`/`unexpected_event` identify the source state and event through
   the flow's nominal `StateId`/`EventId` enums.
2. *Absorbed panic.* A runtime panic inside a transition body (for example a
   division by zero, `E0801`) is absorbed into `Fault` as
   `unexpected_event = Panic { code: ... }`. A panic that occurs while the flow is
   *already* in `Fault` propagates rather than being re-absorbed.

**Recovery is a business-defined state transition.** Recovering from `Fault` is an
explicit transition the author writes — either a transition whose source state is
`Fault`, or a `match` on the `Fault` record that routes to a real business state.
Recovery must not be default-value construction of fake external resources:

```mimi
func handle(f: Fault) -> i32 {
    match f {
        Fault { last_state, unexpected_event, snapshot: _, trace: _ } => {
            match last_state {
                Ready => {
                    match unexpected_event {
                        Panic { code: _ } => { return 1 }  // route to a defined recovery path
                        _ => { return 0 }
                    }
                }
                _ => { return 0 }
            }
        }
        _ => 0
    }
}
```

Recovery rules:

- Anticipated business failures prefer `Result`; they do not automatically enter Fault;
- Invariant breakage, unrecoverable runtime errors, and explicitly absorbed panics can enter Fault;
- Recover must be a business-defined state transition, not default-value construction;
- Reset/recover does not auto-generate business implementations;
- Persistent resources must have an explicit recovery strategy;
- A secondary Fault (a fault while handling a fault) must be recorded or escalated; it cannot be silently swallowed.

> **Deferred to 0.2.** A full fault *set* — a variant block such as
> `fault OrderFault { Storage(StorageError) | Peer(PeerFault) | Timeout(Duration) }`
> with per-variant recover matching — is not part of 1.0. The 1.0 surface is the
> single per-flow typed error (`fault ErrorType`) plus the system payload above.

### 3.13 Progressive Mode `[stable]`

Simple scripts can use Mimi without first learning complete Flow, but implicit Main must be a genuine semantic desugaring.

```mimi
func main() {
    println("hello")
}
```

The compiler must put its real body into the implicit Flow's startup transition, not just insert a shell Flow and continue the traditional main path.

> v0.34.28: **implemented** — `apply_progressive_typestate` (progressive.rs, v0.29.22)
> injects an invisible `flow Main { state Single }` whose `run` transition calls
> the real top-level `main`, so every script lives under the Flow paradigm.
> `bytecode/compiler.rs` "no main function found" fires only when no main exists
> at all (pure library), not as evidence of an unimplemented implicit Main.

Rules:

- Pure synchronous, no persistent resources, no concurrency: script mode allowed;
- Once using Actor, spawn, Session, phased resources, or recover: require explicit Flow or provide applicable migration fix-it;
- CLI can display lowered Flow;
- Diagnostics always map back to user source positions.

---

## 4. Error Model and Debug Prevention

*[source: devdocs/pre-0.1/02-errors-and-debug-prevention.md §2–§12]*

### 4.1 Single Failure Algebra `[stable]`

| Mechanism | Semantics | Propagation Boundary |
|---|---|---|
| `Result<T, E>` | Synchronous, anticipated, recoverable failure | Current call chain |
| `Option<T>` | Value may be absent; no error reason | Current expression/call chain |
| typed `Fault` | Flow can no longer maintain business invariants | Current Flow/supervision tree |
| `PeerFault` | Actor, Session, or concurrent peer fault | Typed peer link |
| `defer` / failure guard | Cleanup or compensation on scope exit | Current lexical scope |
| panic | Programming defect or unrecoverable runtime exception | Default terminates; explicit strategy can absorb to Fault |
| `exit` | Application decides to terminate process | Process boundary |

These channels must not implicitly interchange:

- `Err` does not automatically become Flow `Fault`;
- `Fault` does not masquerade as ordinary `Err`;
- `?` does not exit process;
- `Option::None` does not equal business error;
- Actor runtime errors do not return indistinguishable `0`.

### 4.2 Result, Option, and `?` `[stable]`

`?` is only allowed on expressions with static type `Result<T, E>` or `Option<T>`.

Rules:

- In ordinary functions, `Result<T, E>?` yields `T` on `Ok`, returns compatible error from current function on `Err`;
- In ordinary functions, `Option<T>?` yields `T` on `Some`, returns `None` from current function on `None`;
- Current function return type must be compatible with propagation channel;
- Error conversion must use explicit `From`/mapping rules; must not infer by variant name;
- User enum variants named `Fail`, `Error`, `Err`, or `None` do not get special control flow;
- Interpreter and native backend must execute the same callable-level propagation.

In transition body, `?` can only propagate to the transition's declared rollback failure path, returning typed error and still-valid source generation. It is not ordinary function early return.

#### Prohibited Semantics `[stable]`

1.0 prohibits:

- Codegen printing and exiting process on `?` error path;
- Interpreter judging success or failure by variant name; `[removed]`
- Attempting to take first payload on non-`Result`/`Option` values;
- Implicitly turning `Err` into current Flow's Fault;
- Using global side channel for function propagation.

### 4.3 Rich Fault Sets (Forward Design) `[deferred to 0.2]`
Fault represents a Flow's inability to maintain its state invariants, not a catch-all for all errors.

> **Status.** This section is the **long-term forward design sketch** for a rich
> fault set. It is **deferred to 0.2**; it is not a claim of current 0.1.7
> parser/checker support. The implemented 1.0/0.1.7 surface is the single
> per-flow typed error (`fault ErrorType`) plus the system payload documented in
> §3.12. Readers implementing or using 0.1.7 should follow §3.12, not the
> variant-block syntax below.

The 0.2 forward model may let each Flow declare or derive its own fault set:

```mimi
// 0.2 forward-design sketch; not accepted by the 0.1.7 parser/checker.
fault OrderFault {
    Storage(StorageError)
    Peer(PeerFault)
    Timeout(Duration)
    UnexpectedEvent { state: StateId, event: EventId }
    Panic(PanicPayload)
}
```

> **0.1.7 Phase E note.** The variant-block fault set above remains the
> long-term model but is **deferred to 0.2**. The implemented 1.0/0.1.7
> surface is the single per-flow typed error (`fault ErrorType`) plus the
> system payload in §3.12. This section is retained as the forward design
> sketch, not as a claim of current parser/checker support.

#### Entering Fault

May enter Fault:

- Explicitly declared transition failure that cannot recover as synchronous `Result`;
- Dynamic untrusted boundary receives event not accepted by current state, and boundary strategy chooses fault;
- Peer fault escalation per supervision strategy;
- Watchdog/timeout escalation per Flow strategy;
- Explicitly allowed panic absorption;
- Runtime detection of Flow invariant breakage.

Must not enter Fault:

- Compiler errors (parser, checker, codegen);
- Business failures returnable as ordinary `Result`;
- Statically known illegal transition calls;
- Unimplemented stable backend semantics;
- Type system losing tracking information.

#### Fault Payload

Fault payload at least includes:

- Flow and instance ID;
- Source state and generation;
- Event and resolved transition ID;
- Typed fault/error payload (the single `ErrorType` in 0.1.7; future rich-variant payload in the deferred §4.3 model);
- Source file/span;
- Active resource summary;
- Persistent/transaction state;
- Parent, child, and peer relationships;
- Suppressed secondary faults.

### 4.4 PeerFault and Supervision `[stable]`
`PeerFault` is a typed event propagated across Actor/Session boundaries; should not default to unconditionally converting local Flow to Fault.

Receiver chooses via supervision strategy:

- Ignore and log;
- Return business `Result`;
- Reconnect or restart peer;
- Transfer to degraded state;
- Escalate to local typed Fault;
- Cascade SystemKill.

Circular peer graph must have cycle detection and escalation bound. Repeated faults cannot only keep the first trace and silently swallow subsequent causes.

### 4.5 Dynamic Untrusted Boundary `[stable]`

Static business graph only allows declared transitions. Network, FFI, disk, IPC, deserialization, and dynamic Protocol dispatch cannot be fully statically constrained; must pass through boundary layer:

```text
bytes/dynamic value
  -> decode
  -> schema validation
  -> current-state event validation
  -> typed event
  -> resolved transition
```

Each stage returns independent error:

- `DecodeError`;
- `SchemaError`;
- `UnexpectedEvent { state, event }`;
- `UnknownProtocolMethod`;
- `TransportError`.

Boundary error escalation to Fault is explicitly decided by Flow strategy. Compiler must not generate pseudo business transitions for these errors.

### 4.6 Cleanup and Compensation `[stable]`
1.0 converges to scope guards (0.36.15 表面修正：`defer failure` 无此表面形态，
删除——真实双表面为 `defer { }` 与 `on failure { }`；统一裁决见 Phase D)：

- `defer { }`: execute cleanup whether scope exits normally or abnormally (LIFO);
- `on failure { }`: execute compensation only when scope exits with `Err`, Fault
  absorption, or panic — registered at the statement's execution point, so it
  fires only for failures occurring after it registered;
- Both forms must behave identically on the interpreter and the native backend
  (the resolved codegen registers guard blocks and emits them at function exits;
  the legacy path matches since 0.31.24; pinned by dual-harness tests 0.36.15);
- Transition rollback failure is a failure exit; a `return Target { ... }` terminal is not;
- Ordinary `return Ok(...)` does not trigger failure-only compensation;
- `break`/`continue` trigger rules explicitly defined by lexical scope;
- Compensation failure must aggregate as typed error or secondary Fault; must not overwrite original failure.

Resource exactly-once drop is guaranteed by ownership system; should not depend on user hand-writing compensation for each resource.

### 4.7 Reset and Recover `[stable]`
`reset` and `recover` are business transitions, not compiler auto-generated default-value shortcuts.

#### Reset

Reset destroys state allowed to be destroyed in the fault instance, and enters a specified initial state via a valid constructor.

Must prove or dynamically guarantee:

- Old resources correctly released or transferred;
- New state's required resources genuinely acquired;
- Session/peer/child relationships consistently handled;
- Persistent data retention/discard policy explicit.

#### Recover

Recover uses the explicit typed Fault/error payload and recoverable data to construct target state. In 0.1.7 the typed payload is the single per-flow `ErrorType` described in §3.12; the rich fault-variant form remains deferred to 0.2 (see §4.3).

Must declare:

- Which Faults are accepted;
- Which persistent fields are read;
- Which transactions committed or rolled back;
- How to verify post-recovery invariant;
- What recovery failure returns;
- Whether degradation to reset is allowed.

Undeclared recover-to-reset degradation is prohibited from silently occurring.

### 4.8 Panic Strategy `[stable]`
Panic defaults to programming defect or runtime environment corruption; not part of normal business control flow.

Only explicitly declared Flow/Actor boundaries can absorb specific panics as typed Fault. Absorption strategy must:

- Save original panic type and source location;
- Execute resource handling in safe context;
- Prohibit pretending recoverability after unknown memory corruption;
- Distinguish language panic, FFI signal, OOM, and process abort;
- Behave consistently across both backends.

Compile-time errors can never become runtime panic/Fault.

### 4.9 Prohibit Sentinel Errors `[stable]`
Stable runtime and FFI wrapper prohibit using the same `0`, `null`, empty string, or `-1` to represent multiple failures.

Boundary ABI should use:

- Tagged result;
- Error code + independent payload;
- Or checked handle table structure that cannot collide with success values.

Each error must map to a Mimi type. If C ABI must return sentinel, wrapper must immediately read and convert specific error; sentinel must not enter user language layer.

### 4.10 Fail-fast Compilation Pipeline `[stable]`

```text
Parse accepted
  -> Check resolved and supported
  -> Lowered typed IR
  -> Interp/Codegen consume same resolution
```

Prohibited:

- Checker losing information then "best effort" skipping checks;
- Codegen unsupported only warning and continue;
- Codegen re-guessing transition or failure variant;
- Runtime discovering errors that could have been statically determined;
- Solver Unknown treated as verification success;
- Unknown attribute silently ignored.

Backend capability gaps should report stable diagnostics at checker's capability gate, pointing to experimental feature flag or migration path.

---

## 5. Verified Core

*[source: devdocs/pre-0.1/03-verified-core.md §1–§14]*

### 5.1 Definition `[stable]`

Mimi 1.0 does not claim to use Z3 to verify the complete Mimi language.

The stable product of static verification is:

> Mimi Verified Core 1: Generate and solve verification conditions for a versioned, pure, machine-semantics-precise typed verification IR.

Programs not in the trusted subset can still pass type checking and use runtime contracts, but must not receive `Proven`.

### 5.2 Trusted Abstraction Rules `[stable]`

Verified Core only accepts abstractions precisely corresponding to Mimi execution semantics:

- `i32/i64` use checked integer semantics and generate definedness obligation;
- `f64` must use IEEE-754 FloatingPoint model; reject before entering Verified Core;
- Function calls can only be summarized when callee belongs to the same pure/total proof profile;
- Control flow must first lower to CFG/SSA; no erasing branch, loop, spawn, or await;
- Heap, Flow, Actor, Session, Fault, resources, and concurrency only proven by explicitly versioned dedicated logic;
- Unsupported nodes return `NotInTrustedSubset` before SMT encoding.

### 5.3 Architecture Boundary `[stable]`

```text
Source
  -> parse
  -> type/effect check
  -> resolved typed IR
  -> trusted-subset gate
  -> Verification IR
  -> CFG
  -> SSA
  -> verification conditions
  -> SMT
  -> structured outcome
```

Verifier must not give stable `Proven` directly from untyped raw AST.

### 5.4 Verified Core 1 First Version Scope

#### Allowed Types `[stable]`

- `bool`;
- `i32`;
- `i64`.

Default integer model is **Checked Integer**:

- Each i32/i64 operation generates input/output range and no-overflow obligation;
- Division/modulo generates divisor-non-zero and `MIN / -1` definedness obligation;
- Interpreter, native codegen, constant folding, comptime, and verifier use the same semantics;
- Overflow, division by zero, and undefined operations produce the same typed arithmetic error;
- Code requiring wrapping must use explicit `wrapping_*` operations; does not change default integer semantics.

#### Allowed Expressions `[stable]`

- Scalar literals;
- Immutable scalar parameters;
- Restricted `old(param)`;
- Arithmetic with defined machine semantics;
- Comparison and boolean operators;
- Pure, exhaustive, finite `if`/match;
- Side-effect-free let binding;
- Single return expression or fully CFG/SSA-ed finite branching.

#### Allowed Functions `[stable]`

- Synchronous;
- Pure;
- Total, or all partial operations have definedness obligation;
- No mutation;
- No loop;
- No recursion;
- No allocation;
- No panic;
- No hidden global state;
- No concurrency;
- No FFI.

### 5.5 Explicitly Prohibited in First Version `[stable]`

The following get `NotInTrustedSubset`, cannot become abstract variables:

- `f32/f64`;
- String, List, Map, Set;
- Record field and heap;
- Reference, pointer, shared/weak;
- Mutation and `mutate` parameter;
- Loop and recursion;
- Arbitrary user/builtin call;
- Time, random, I/O, network;
- FFI and unsafe;
- Allocation;
- `spawn/await` and async;
- Actor, Flow transition, Protocol dynamic dispatch, Session;
- Mutex, Atomic, Channel;
- Comptime and generated code;
- Closure/lambda;
- `old` on aggregate/alias;
- Unknown or erased types.

### 5.6 Contract Language `[stable]`

`requires`, `ensures`, and `invariant` should not reuse all executable `Expr`.

Stable specification expressions must:

- Have no side effects;
- Be total;
- Only call approved pure logic functions;
- Not read time, random, I/O, or mutable global;
- Not allocate, not spawn, not call FFI;
- Have types and definedness fully encodable.

Contracts should be exclusive fields of function/transition definitions, not ordinary statements at arbitrary block positions.

#### `old` `[stable]`

Verified Core 1 only allows:

```mimi
old(immutable_scalar_parameter)
```

Prohibits field, List, shared, pointer, or alias aggregate `old`.

#### `math` `[stable]`

General `math { Expr... }` blocks are a **stable** mathematical-annotation channel: parsed as
`Stmt::Math` (parse_stmt.rs:896/1006), consumed by the verifier as ghost facts
(vir.rs:495/878, `ResolvedStmtKind::Math`). No ghost-extension beyond the
verifier channel is planned.

### 5.7 Result States `[stable]`
Stable results must not only use `Verified/Failed/Unknown`:

| State | Meaning |
|---|---|
| `Proven` | Proven under declared Verified Core semantics |
| `Disproven` | Counterexample found in trusted model |
| `NotInTrustedSubset` | Program uses unmodeled constructs |
| `SolverUnknown` | Logic supported but solver cannot decide |
| `Timeout` | Exceeded solving budget |
| `InfrastructureError` | Solver missing, crashed, or IPC/loading failed |
| `RuntimeOnlyContract` | Contract can only be dynamically checked |
| `NoObligations` | No static proof obligations |

Command success condition: all obligation requests are `Proven` or `NoObligations`. Other states must not be let through `verify` gate after a warning.

### 5.8 Fail-closed Rules `[stable]`

- Z3 unavailable returns `InfrastructureError`, not mock Unknown;
- Unsupported returns `NotInTrustedSubset`, does not create fresh variable;
- Caller requires cannot encode → caller cannot `Proven`;
- Solver Unknown/timeout must propagate to obligation;
- Solver panic/crash fails current proof session;
- `build --verify-ffi` only continues if all required call sites are `Proven`;
- Advisory behavior must use different name, e.g., `--audit-ffi`;
- Public API defaults to typed IR only; raw-AST verifier must be explicitly test/experimental.

### 5.9 Proof Output `[stable]`

Each `Proven` must declare:

```text
verification semantics: mimi-verified-core-1
integer model: checked-int-v1
float model: forbidden
heap model: none
calls: none or pure-dag-v1
termination: structural/not-applicable
runtime assertions elided: no
solver and version: ...
source/IR hash: ...
```

### 5.10 Extension Order `[stable]`

1. bool and precise i32/i64;
2. Pure finite branching;
3. Pure acyclic calls;
4. Immutable algebraic data;
5. Arrays/List element and length model;
6. Bounded loops with complete invariant;
7. Termination;
8. IEEE FloatingPoint;
9. Heap/alias/separation logic;
10. Flow transition relation;
11. Concurrent dedicated model checking or rely/guarantee.

---

## 6. Language Coherence Decisions

*[source: devdocs/pre-0.1/04-language-coherence.md §2–§14]*

### 6.1 Functions: `func` and `fn` `[stable]`

- `func name(...)`: named function definition;
- `fn(...) { ... }`: anonymous closure;
- `fn(T) -> U`: function value/function pointer type;
- `extern "C" fn(T) -> U` / `extern "C" func(...)`: FFI declarations — both spellings accepted.

Memory rule: named functions use `func`; functions-as-values use `fn`.

> **ADR-003 裁决（2026-08-02）**：`fn` = 闭包（表达式位置）、`func` = 函数声明、
> extern 类型两者皆可。**保留现状**，不强制收敛。`func(T) -> U` 作为函数类型
> 仍解析（parse_type.rs:213-237）；`fn` 用于闭包表达式与 `extern "C" fn(...)` 类型
> （parse_type.rs:242）。原 "Convergence [removed]" 节废止，见 golden-document §1.2。

### 6.2 Permissions: `view/mutate/consume` vs `&/&mut` `[stable]`

Mimi user-level safe API only uses:

```mimi
func inspect(x: view T)
func update(x: mutate T)
func take(x: T)          // by-value consume
```

`&T`, `&mut T`, explicit lifetime, and `*ptr` do not enter the stable preferred syntax for ordinary safe business code. They may only exist in:

- `unsafe`;
- FFI wrapper;
- Runtime/low-level library;
- Explicitly feature-gated advanced mode.

#### `mutate` argument place grammar `[stable]`

> v0.34.25c (E0434/E0435): `mutate` arguments are restricted to real places
> with an exclusive-borrow invariant, so silent write-back loss is impossible.

- Legal `mutate` argument: `Ident` or single-level `Ident.field` (including `self.field`) — exactly the write-back targets the checker can coerce back into the payload slot after the call.
- Rejected (`E0434`): nested places (`o.inner.value`), non-place arguments (literals, computed values, `bump(42)`), index expressions;
- Rejected (`E0435`): two `mutate` arguments within the same call aliasing the same place (`bump2(self.tag, self.tag)`) — violated exclusive borrow.
- Nested write-back and cross-call alias tracking are deferred to 1.x (require a backend place-tracking mechanism).

#### Task-boundary narrow `[stable]`

> 0.1.8 Phase A (`E0442`): `view` / `mutate` / `&T` / `&mut T` are task-local.
> They must not enter `spawn`, Channel elements, Future captures, or actor
> mailboxes. Synchronous `func` parameters (including the DSP `mutate List`
> hot path) are not a task boundary and stay legal.

### 6.3 Ownership: Flow payload and shared/weak `[stable]`

- Flow payload defaults to exclusive, linearly transferred by transition;
- `shared` indicates explicit shared object graph;
- `weak` indicates non-owning reference to shared object;
- On Fault/drop Flow payload, only drop shared handle; do not assume destroying shared object.

#### Convergence `[removed]`

- Silently treating `shared T` as `T` in return type checking: **removed**;
- Flow payload's shared wrapper must not be implicitly unwrapped;
- `shared x = expr` / `weak x = expr` lower to unified `let` + constructor:
  ```mimi
  let value: Shared<T> = shared(expr)
  let observer: Weak<T> = weak(value)
  ```

### 6.4 State: Flow and Actor `[stable]`
- Flow is the sole model for business state and change;
- Actor is Flow's concurrent runtime container;
- User-visible `mut` actor fields are **removed** (0.1.8 Phase D, `E0402`): the
  SD-5 simple-state escape hatch is closed. Non-`mut` per-instance metadata
  fields remain writable on every backend;
- `actor Name runs FlowName` is the supported business-actor shape: state must
  be carried by the Flow's payloads (atomic-turn guarantee);
- Actor mailboxes and sync methods perform per-instance field access; business
  state *change* belongs to Flow transitions (migrate mailbox method calls to
  typed Flow events);
- Actor runtime holds unique Flow instance.

#### TransitionEpoch and boundary packing `[stable]`

> 0.1.8 Phase C (`E0443`): a Flow value conceptually carries a `TransitionEpoch`.
> Crossing a task boundary requires an explicit `flow_pack` handle; a peer that
> still holds an older epoch receives a typed stale error instead of a silent
> alias or use-after-free.

- Bare Flow records cannot cross Channel, FFI, or an actor mailbox (`E0443`);
  use `flow_pack` to publish a packed TransitionEpoch.
- Local self-loops stay inside the same turn/actor (clause 5.1 silent stay) and
  strip the epoch with no packing tax; `flow_pack_count` does not increase.
- `flow_epoch` reads the live epoch, `flow_check_epoch` verifies a peer's
  expected epoch, `flow_bump_epoch` publishes a recovered epoch, and
  `flow_unpack` consumes a packed payload, and `flow_drop` releases a packed
  handle so later use returns a typed stale error. A stale check returns
  `EPOCH_ERR_STALE` (2). (`FLOW-EPOCH-DROP-001` indexes this `flow_drop`
  release/stale contract; see the Normative Requirement Map §6.4.)
- `flow_pack_count` reports the number of live packed handles (debug/diagnostic);
  `flow_epoch_last_error` returns the last `EpochError` code (`EPOCH_OK` 0,
  `EPOCH_ERR_INVALID` 1, `EPOCH_ERR_STALE` 2, `EPOCH_ERR_BARE_RECORD` 3) for
  the current thread.

### 6.5 Abstraction: trait and Protocol `[stable]` / `[removed]`

- `trait`: stateless value interface;
- `session`: communication endpoint message ordering.

`protocol` 语言表面已删除（0.1.7 Phase E）：Flow 的状态/transition 拓扑直接
构成其外部接口；不再提供 `protocol` 声明或 `impl ProtocolName` 投影语法。
静态组件边界 Protocol 作为外部 ABI/schema 概念保留。

### 6.6 Session `[stable]` / `[experimental]`

Session enters stable set only after:

- `session_pair::<P>()` returns `SessionChan<P>` and `SessionChan<dual P>`;
- Endpoint is a linear value;
- send/recv/close advance residual;
- Alias, fields, return, closures, and branch merge preserve tracking;
- Untracked reports error;
- No bare `List<i64>` or integer handle user API;
- Interpreter/native runtime behavior consistent.

#### Session method surface `[0.1.8 Phase E]`

- `ch.send(v)`, `ch.recv()`, and `ch.close()` are the canonical method form;
- The method form advances the same residual/order proof as the free
  `session_send`/`session_recv`/`session_close` functions;
- Free session functions are deprecated (`W014`) and emit a migration hint;
  new teaching and dogfood code should use `ch.send`/`ch.recv`/`ch.close`.

Recursive protocols, dynamic participants, delegation, multiparty Session, and cross-version residual upgrade remain experimental.

### 6.7 Transition body and `do` `[removed]`

Remove the semantically empty `do` wrapper. Transition's `{ ... }` is itself the implementation body:

> v0.34.27: executed — `Stmt::Do` removed from the AST (~28 points / 15 files),
> `do` dropped from the keyword table (81 → 80), 24 real_world + test corpus
> migrated (`{ do { X } }` → `{ X }`). A bare `do` identifier followed by `{`
> now parses as a struct constructor for an undefined type and is rejected by
> the checker.

```mimi
transition ship(Paid) -> Shipped
    fails TrackingError
{
    let tracking = allocate_tracking()?
    return Shipped { tracking }
}
```

If body uses `?`, signature must declare rollback failure error type; failure returns source generation.

> v0.34.11 (ADR-001): the terminal above is spelled `return Shipped { ... }`
> (`become` removed). Self-loops spell `return SourceState { ... }` explicitly.

### 6.8 Contracts: requires, ensures, invariant, math `[stable]`
#### Function-exclusive structure

Contracts are exclusive fields of function/transition definitions:

```mimi
func withdraw(balance: i64, amount: i64) -> i64 {
    requires: amount >= 0
    requires: amount <= balance
    ensures: result == balance - amount
    balance - amount
}
```

> **语法注意（0.35.22 修正）**：合约语句写在**函数体内**，关键字后带冒号
> （`requires: ...` / `ensures: ...`）。函数头行尾 `requires amount >= 0`
> （既无体内位置也无冒号）的旧写法会被 parser 拒绝（E0500 系列）。
> 校验：`docs/syntax-reference.md` golden 语法与 `devdocs/v0.31/04-type-system.md`
> 裁决文档一致。

#### `invariant`

- Flow state invariant belongs to state/Flow declaration;
- Loop invariant belongs to loop header;
- Function invariant if no independent meaning: not retained;
- Runtime and static verifier check timing must be explicit and dual-backend consistent.

#### `math` `[stable]`

General `math { Expr... }` blocks are a stable verifier channel (see §5.6).
0.34.28: corrected from an erroneous `[removed]` marking (golden §1.1 verdict).

### 6.9 MimiSpec Meta-syntax: `desc`, `rule`, `mms` `[removed]`

- **0.1.8 Phase E**：`.mms` / `mimi mms` / the external `mimispec` parser are
  **removed** from the compiler. `mms {}` is a hard parser error;
- Production `.mimi` `desc`/`rule` statements: **removed** from stable syntax;
- If needing to associate external intent, use documentation metadata, trivia attachment, or external mapping;
- Unrecognized metadata must warning/error; must not pretend verified.

### 6.10 Comptime `[stable]`

#### `comptime`

- Only calls comptime or explicitly pure functions;
- Prohibits I/O, spawn, Actor, FFI, shared mutation;
- Return value must serialize to compile-time constant;
- Effect/purity enforced by checker;
- Evaluation failure is hard error;
- Runtime not generating comptime symbol is normal; no misleading warning.

> **0.1.7 Phase E 已删除**：`quote` / `quote!` / `$(...)` 语法面已从语言移除
> （`feature-design-review-0.37.md` #1）。`comptime` 常量折叠保留。

### 6.11 Attribute and Keywords `[stable]`
- Unknown attribute, repr, annotation: default hard error;
- Reserved attribute must give "reserved but not implemented" diagnostic;
- Soft keyword only used when there is genuine identifier compatibility need;
- Same token must not assume unrelated semantics (e.g., `|>` not both pipe and transition union separator);
- Multi-target only uses `|`;
- User Flow transition must have body; Protocol transition signature allows body-less;
- System injected nodes distinguished by AST flags, not disguised as user-writable empty body.

### 6.12 Stable / Experimental / Removed Checklist

#### Stable targets

- `func` definition, `fn` value/type;
- Flow state/transition;
- Linear state identity;
- Result/Option/match/`?`;
- view/mutate/consume;
- Actor runs Flow;
- Minimum dual-end typed Session;
- Typed Fault/PeerFault;
- Function-exclusive contracts;
- Restricted pure comptime;
- Multi-target transition `-> A | B`（tagged-union ABI，0.34.15-16，ADR-002；从 Experimental 升入 Stable，0.34.28 裁决同步战役补齐）;
- `math` verifier ghost channel（§5.6/§6.8，verifier 通道，非执行 AST）;
- Effect/capability is not a stable target in this profile; its proposed minimum model remains experimental under `EFFECT-CAP-001`.

#### Experimental

- In-process FFI signal recovery and forced thread termination;
- Compiler auto-synthesized recover (explicit typed reset/recover is stable);
- Heterogeneous Actor collection;
- Explicit low-level references outside runtime/FFI;
- Higher-order effect polymorphism.

#### Removed / Migrated

- Semantic-less `do`（v0.34.27 已删除）;
- `delegate view/mutate/consume`（amendment clause 2）;
- `@transactional` / WAL / metadata_shadow（amendment clause 3）;
- `pinned(timeout)`（amendment clause 10）;
- `|>` as transition target separator;
- Explicit lifetime `&'a T`（ADR-004）;
- `.mimi` `desc`/`rule` statement;
- Executing AST `mms` statement;
- String-based Protocol reflection;
- Shared wrapper auto-stripping;
- Failure variant name heuristics;
- `?` process exit;
- User-visible bare Session `i64` handle;
- Unknown attribute silent ignore.

> **0.36.13 修正（已被 0.1.8 Phase D 进一步推翻）**：本条原称 SD-5 保留 `mut` 为
> 简单状态逃生舱；0.1.8 Phase D 废止 SD-5，**任何**业务 `mut` 字段（含
> `actor runs FlowName`）都被 E0402 拒绝，业务状态只许活在 Flow 中。本条原叙述不再成立。

> **v0.34.28 修正**：`math { Expr... }` 不在此清单——它是 **stable** verifier 通道
> （§5.6/§6.8），此前误列 Removed，同步战役纠正。

> **v0.34.6 修正（ADR-003）**：`func(T) -> U` 函数类型与 `extern "C" func(...)`
> **保留**（非 removed）——`func` = 声明、`fn` = 闭包、extern 两者皆可。
> 此项从 Removed 清单移除，见 §6.1。

### 6.13 Numeric Coercion `[stable]`（v0.34.6，golden §2.1）

Mimi 数值隐式转换**只允许单向 widening**：

| 源 → 目标 | 隐式允许 |
|-----------|---------|
| `i32` → `i64` | ✅ |
| `i32` → `f64` | ✅ |
| `i64` → `f64` | ✅ |
| `i64` → `i32` | ❌ 需显式 `as i32` |
| `f64` → `i32`/`i64` | ❌ 需显式 `as` |
| `i32` → `f64` 之外的任何窄化 | ❌ |

规则：
1. 隐式转换仅发生在**声明位置**（变量绑定、函数实参、variant payload）的
   `is_numeric_coercion` 检查（core/helpers.rs:354-371）。
2. **窄化必须显式 `as`**：`str_parse_int(...) as i32`（先例 prelude.mimi to_int_safe）。
3. `freeze_variant_payload` 不豁免双向数值（infer_expr.rs:174-200）——变体 payload
   只接受单向 widening，窄化报 E0209。
4. 语义确定性：状态机数据必须数学自洽（架构修正案），隐式窄化可能静默截断，
   因此禁止。Z3 合约可证明无溢出时消除运行时检查。

### 6.14 Module System: `use` Merge, Naming Self-Description, and `::` Reservation `[stable]`（0.39.137 裁决）

Mimi 的模块系统是**文件级 merge 模型**，不是路径限定调用模型：

| 面 | 规则 |
|----|------|
| `use std::fs;` | 加载 `std/fs.mimi`，其 `pub` 导出（函数/类型/trait 实现）**以裸名合并**进当前作用域 |
| 裸名调用 | 合并后的函数直接 `write(...)` 调用（AGENTS §5.2 记载的主路径） |
| 重复导出 | 两个已导入模块导出同名项 → **fail-loud duplicate item 错误**（不静默遮蔽） |
| `M::f(...)` 前缀调用 | **不支持**——checker 以 E0400/E0221（内联 module）/TOOL-RESOLUTION-001（文件模块）拒绝 |
| 内联 `module X { ... }` | 解析器接受、声明可注册，但其函数**不可被任何语法调用**（死面，见下） |

裁决理由：
1. **命名自描述替代限定符**：stdlib 以领域前缀承担出处职责
   （`write_file`/`str_split`/`map_new`），调用点可读性由命名约定保证，
   无需第二套解析规则。collections 内联成员判断规避 strings::contains、
   `remove_at/remove_value` 避开 set/maps 是此约定的既有实践。
2. **确定性解析**：单一解析规则 + duplicate fail-loud = 零静默遮蔽。
   双轨（merge+prefix）要求 checker/VM/codegen/LSP/fmt 五面同步两套路由，
   违背最小心智模型。
3. **`::` 的语义身份留给业务状态机**：`FlowName::transition(state, ...)`
   是 Mimi 的核心差异化——显式的状态转移边。工具函数不承载状态语义，
   不占用该符号。
4. **同语义重复对是允许的**（text↔strings、time↔datetime、env↔json 的
   `get_int` 等）：它们互斥导入即可，duplicate 错误本身就是提示。

已知边界（0.39.137 登记，0.39.138 收紧）：
- 内联 `module` 块**检查期硬拒绝（E0445）**：其中的项任何语法都调不到
  （native codegen 从未编译；VM 分发管道 0.39.137 删除），静默放行声明
  违反 §4.10 fail-fast。文件模块是唯一模块形式。`module` 关键字与残余
  AST 管道退役立案为 pre-1.0 清理项（1.0 API 冻结前必须完成）。
- 异语义同名陷阱须改名消解：`csv::get` → `csv::cell`（0.39.137，
  与 maps::get 冲突且语义完全不同）。

---

## 7. Component Boundary, Native ABI, and Wire Schema

*[source: devdocs/pre-0.1/07-first-class-ffi.md §1–§21]*

### 7.1 Definition `[stable]`
Component Boundary is a first-class citizen of Mimi 1.0. FFI is its in-process native transport; IPC, WebSocket, and worker process use wire transport. Both share Protocol, Session, error, capability, and trace semantics, but native ABI tokens, pointers, and allocators never enter wire.

Core principle:

> External languages can implement operations and ecosystem capabilities, but cannot extend a set of business state semantics that bypasses Flow.

### 7.2 Two-layer Boundary `[stable]`
#### Raw ABI Layer

For runtime, generated shim, and explicit `unsafe` adapter:

- C ABI;
- Bare pointers;
- Platform calling convention;
- `repr(C)`;
- Foreign exception/signal capture;
- Allocator glue.

Ordinary business code cannot directly access Raw ABI Layer.

#### Typed Component Layer

For ordinary Mimi and generated SDK:

- Typed handle;
- Result/Fault;
- Ownership permission;
- Protocol event;
- Session endpoint;
- Async task/subscription;
- Capability;
- ABI/schema version;
- Trace context.

Each raw import must be enclosed by typed wrapper.

Raw extern itself is not a stable application surface. `COMPONENT-RAW-001` restricts it to explicit `unsafe` adapters or an experimental escape profile; placing a declaration in a manifest does not promote it. Stable import/export declarations are typed Component IR surfaces.

### 7.3 Component IR is Single Source of Truth `[stable]`
```text
Typed Mimi IR
  -> Component IR / .mimiabi
     -> C header
     -> Rust -sys crate
     -> Rust safe crate
     -> Node addon
     -> TypeScript declarations
     -> Python/Java/Swift adapters
     -> ABI checker
     -> documentation
     -> conformance tests
```

`.mimiabi` at least includes:

- Component identity and semantic version;
- ABI version and target assumptions;
- Imports/exports;
- Type layout/schema hash;
- Symbols;
- Ownership, nullable, and destructor;
- Errors;
- Effects;
- Callback/async policy;
- Thread affinity;
- Capabilities;
- Flow/Protocol/Session projection;
- Since/deprecated;
- Trace policy.

### 7.4 Native Cross-boundary Types `[stable]`
| Type | Semantics |
|---|---|
| `ffi view T` | Read-only, valid only for synchronous call dynamic scope |
| `ffi mutate T` | Exclusive during call; no save, no release, no realloc |
| `ffi owned T` | Ownership moved to receiver |
| `ffi shared T` | Typed shared handle; explicit clone/release |
| `ffi weak T` | Does not extend lifetime; can upgrade |
| `ffi handle T` | Opaque resource handle |
| `ffi slice<T>` | ~~`{ptr,len}` read-only view~~ **REMOVED (0.1.7)** |
| `ffi slice_mut<T>` | ~~`{ptr,len}` exclusive view~~ **REMOVED (0.1.7)** |
| `ffi str` | UTF-8 `{ptr,len}` view |
| `ffi owned_str` | Owned UTF-8 string with allocator/destructor |
| `ffi c_str` | Explicit NUL-terminated C string |
| `ffi buffer<T>` | ~~owned `{ptr,len,cap,allocator}` buffer~~ **REMOVED (0.1.7)** |

> **已判死删除（0.1.7 Phase E）**：`ffi slice`/`ffi slice_mut`/`ffi buffer`
> 为纸面特性—— parser/codegen 零支持（M-009 实证），0.1.7 起从特性表移除；
> 不进入 1.0 语言表面。与指针读写同属 0.2 Component IR 特性轨
> （见 `devdocs/v0.34/dogfood-jupitune-eval-0.34.34.md` §3 登记）。
> 语言层 `view/mutate/consume` 权限模型不受影响。

The `ffi view/mutate/owned` are Component IR ABI modes. Mimi surface language continues to only use `view/mutate/consume`; no parallel permission mental model.

### 7.5 Handle `[stable]`
Each opaque handle must associate:

```text
component_id
type_id
slot
generation
owner_runtime
permission
lifecycle_state
state/session epoch
```

All operations validate: runtime ownership, type, generation, move/release state, thread, Flow state, capability scope.

Stable handle ABI must freeze token bit-width, kind/type/protocol ID, slot, generation, runtime instance, permission, and null semantics. Slot release must promote generation and delay reuse; generation must not silently wrap; old token never re-valid after process restart.

`StaleGeneration`, `WrongHandleType`, `WrongRuntime`, and `ClosedHandle` must be different errors.

Handle lookup must return a concurrent lease/guard. The object lifecycle is `Alive -> Closing -> Dead`: `Closing` rejects new leases, and physical release occurs only after the last in-flight lease ends. Child handles and borrowed views are bound to the parent slot and generation and become stale after parent transition, recover, reset, close, or kill.

### 7.6 Error and Wire Envelope `[stable]`
```text
BoundaryResult<T, E> =
    Ok(T)
  | Error(E)
  | ForeignPanic(PanicPayload)
  | ForeignException(ExceptionPayload)
  | Cancelled(CancelReason)
  | Timeout(Deadline)
  | AbiMismatch(AbiEvidence)
  | RuntimeUnavailable(RuntimeEvidence)
```

Prohibits ambiguous `0/null/-1`.

### 7.7 Callback `[stable]`
- Scoped callback: only callable before synchronous foreign call returns;
- One-shot callback: exactly one terminal invocation;
- Subscription callback: long-lived; returns linear Subscription;
- Send callback: allows cross-thread; via runtime event queue;
- Main-thread callback: dispatched to specified event loop.

Long-lived callbacks return a linear subscription. Closing a subscription requires foreign quiescence confirmation and drains in-flight callbacks before releasing captured resources. A late callback is rejected as `StaleGeneration`; it cannot enter a newer Flow generation.

### 7.8 Async and Cancellation `[stable]`
Async operations return linear `ForeignTask<Pending, P>`, not untracked Promise/Future.

Minimum Session:

```text
Pending:
  receive Completed(Result<T,E>) -> End
  send Cancel(reason) -> Cancelling

Cancelling:
  receive Cancelled -> End
  receive Completed(Result<T,E>) -> End
```

Rules:

- Cancel request ≠ cancel completion;
- Terminal outcome exactly once;
- Task-owned borrow/pin/callback/capability not released before terminal acknowledgement;
- Late completion rejected by generation or recorded;
- Non-cancellable operations marked `non_cancellable`; must drain or process-isolate;
- Timeout is Session event;
- Detached task must have supervisor.

### 7.8.1 Wire Contract `[stable]`

Wire transport uses a canonical, versioned envelope with stable component, Protocol, message, field, error-variant, request, revision, and trace identities. It must define unknown-field/tag behavior, size/depth limits, duplicate and out-of-order handling, replay policy, revision conflicts, and schema handshake.

Native handle tokens, process pointers, allocator identities, callback contexts, and native layout bytes are never valid wire values. Wire capabilities are revocable proxy credentials constrained by scope and audience, not serialized native handles.

### 7.9 Flow Typestate Projection `[stable]`
Flow exports opaque handle + dynamic state check to C; generates typestate wrapper to languages with type systems.

Rust:
```rust
Order<Draft>::submit(self, args) -> Result<Order<Submitted>, SubmitError>;
```

TypeScript:
```ts
interface DraftOrder {
  readonly state: "Draft";
  submit(args: SubmitArgs): Promise<SubmittedOrder>;
}
```

External code cannot read or write private Flow payload; can only receive versioned immutable projection.

---

## 8. Multi-language Strategy

*[source: devdocs/pre-0.1/06-multilanguage-strategy.md §1–§12]*

### 8.1 Core Positioning `[stable]`

Mimi 1.0 is the business state and reliability core of a multi-language system, not a closed full-stack language. Component Boundary is the upper abstraction: in-process FFI uses native ABI profile; IPC, WebSocket, WASM, and worker process use wire profile; both share Component Contract.

### 8.2 System Layering `[stable]`

```text
GUI / Product Surfaces (TypeScript · Swift · Kotlin)
    ↕ generated typed SDK
Mimi Component Boundary (Protocol · Session · ownership · error · async · version)
    ↓
Mimi Reliability Core (Flow · Actor · contracts · capability · Fault/Recover)
    ↓ typed foreign capabilities
Native / Ecosystem Components (Rust · C/C++ · Python · Go · Java)
```

### 8.3 Three Meanings of Authority `[stable]`

- **Fact authority**: Payment networks, databases, devices, OS are responsible for external facts.
- **Business state machine authority**: Mimi Flow is responsible for business state, legal events, and transitions.
- **View authority**: GUI is responsible for uncommitted interaction state; committed business projection comes from Mimi.

External facts must carry source, version, idempotent key, and necessary causal information to enter Flow as typed observation. Flow does not forge external facts, nor does it directly treat unvalidated external facts as business state.

### 8.4 Rust's Responsibility `[stable]`

- OS and hardware interface;
- Drivers and embedded HAL;
- High-performance network, database, storage, and crypto adapters;
- SIMD, GPU, image, audio/video, and compression;
- Safe wrappers for C/C++ libraries;
- Components needing unsafe, fine memory layout, or mature crate ecosystem;
- Mimi runtime and platform glue.

Rust adapter implements effects; does not own business state.

### 8.5 TypeScript's Responsibility `[stable]`

- Web, Electron, and cross-platform GUI;
- View layer state;
- Forms, animation, routing, and interaction;
- Browser and Node ecosystem;
- Optimistic UI temporary projection.

GUI and Mimi use command + immutable projection:

```text
TS -> typed command -> Mimi Flow
Mimi -> accepted/rejected/fault -> TS
Mimi -> versioned snapshot/event -> TS view
```

### 8.6 Prevent Distributed Monolith `[stable]`

- Split by autonomous capability, not by individual function;
- High-frequency fine-grained operations should batch or move into same component;
- Core business commit point must occur within Mimi transition;
- External components only know business through public Protocol; do not read private state tag;
- Components can independently handshake, close, restart, and upgrade;
- Component faults identified and handled by Mimi supervisor;
- No cross-runtime sharing of unversioned mutable object graph.

### 8.7 Prohibit Dual-master State `[stable]`

Any business fact has only one commit authority. Mimi business state authority is a Flow generation; database records, payment ledgers, device state, or client offline documents can have external authority, but Flow must hold typed identity/revision and explicitly handle conflict, unavailability, and compensation.

- Rust adapter does not cache writable business state;
- TS store only saves projection and speculative state;
- Python worker does not own business transaction state;
- External database as persistent fact source: Mimi Flow must explicitly version, transact, and conflict-resolve;
- All commands carry expected generation/revision;
- Stale command returns typed conflict.

---

## 9. RC Acceptance Conditions

*[source: devdocs/pre-0.1/05-rc-migration-and-gates.md §4, §12]*

### 9.1 P0 RC Blockers

#### Flow and Actor

- Flow state freely forgeable or copyable: **blocks RC**;
- Old state usable after transition: **blocks RC**;
- Codegen transition resolution has first-candidate fallback: **blocks RC**;
- Actor mutable business field bypassing Flow: **blocks RC**;
- Undeclared business combination default-injected as transition: **blocks RC**;
- Dynamic event without typed boundary error: **blocks RC**;
- A build that lowers multi-target transitions while losing the state tag or silently selecting the first target violates the specification and **blocks RC**; tagged-union lowering (0.34.15-16) is the only accepted lowering;
- Interpreter/native Actor lifecycle semantics differ: **blocks RC**;
- Async Actor API claims static knowledge of arrival state, or no stale generation/revision typed result: **blocks RC**;
- Transition rollback failure without declared error type, or source generation ownership uncertain after failure: **blocks RC**.

#### Errors

- `?` behaves differently across backends: **blocks RC**;
- Failure depends on variant name: **blocks RC**;
- Runtime user-visible error uses indistinguishable sentinel: **blocks RC**;
- Result/Fault/PeerFault/exit implicit conversion: **blocks RC**;
- reset/recover silent default-constructs resources or degrades: **blocks RC**;
- Compensation trigger rules differ across backends: **blocks RC**.

#### Session and Resources

- Session endpoint can degrade to bare integer: **blocks RC**;
- Untracked endpoint skips check: **blocks RC**;
- Non-`end` endpoint leaves scope without diagnostic: **blocks RC**;
- Resource cannot prove exactly-once on branch/Fault/transition: **blocks RC**;
- Transactional recovery only in interpreter while codegen warning passes: **blocks RC**.

#### Verifier

- Known unsound construct still `Verified/Proven`: **blocks RC**;
- Integer semantics inconsistent with execution backends: **blocks RC**;
- Arbitrary call/spawn/await erased or fresh-variable'd: **blocks RC**;
- Verifier directly accepts untyped raw AST as stable entry: **blocks RC**;
- Unknown/timeout/infrastructure failure let through: **blocks RC**;
- `build --verify-ffi` fail-open on Unknown: **blocks RC**;
- Product documentation claims full formal verification without boundary: **blocks RC**.

#### Tool Consistency

- Parser/checker accepts stable but codegen unsupported: **blocks RC**;
- Unknown attribute silently ignored: **blocks RC**;
- Formatter/LSP does not understand stable syntax: **blocks RC**;
- Documentation, manifest, implementation, or test conflict on stability claims: **blocks RC**.

#### Multi-language and Component Boundary

- Any raw extern exposed as stable, even when listed in a Component manifest, **blocks RC**; stable import/export surfaces are typed Component IR declarations, while raw extern remains an explicit unsafe or experimental adapter layer;
- C/Rust/TypeScript binding each interprets type or ownership differently: **blocks RC**;
- Handle missing kind/type/generation/runtime owner: **blocks RC**;
- Handle lookup/destroy without concurrent lease: **blocks RC**;
- Callback without quiescence confirmation: **blocks RC**;
- Async cancel without exactly-one terminal outcome: **blocks RC**;
- ABI/schema/Protocol without version handshake: **blocks RC**;
- Allocator provenance or error payload ownership unclear: **blocks RC**;
- Flow/Actor/Session degrading to bare integer across boundary: **blocks RC**;
- GUI and Mimi simultaneously hold writable business state: **blocks RC**;
- Cross-language trace cannot correlate Flow generation and foreign task: **blocks RC**.

### 9.2 Completion Conditions

This section defines RC acceptance conditions, not current progress. Current evidence is reported by `docs/language-support.toml`, automated probes, and the release report.

RC requires all of the following:

- The specification, requirements manifest, support evidence, tests, and documentation agree;
- Typed resolved IR is consumed by both backends;
- Flow instances are linear, qualified, and unforgeable;
- Actor business state is carried by Flow;
- Sparse business graph replaces default N×M Fault completion;
- Result/Fault/PeerFault/exit layering is closed;
- view/mutate/consume is the sole stable safe permission model;
- Minimum dual-end typed Session residual is closed on all paths; advanced Session remains experimental;
- Resource exactly-once is closed across transition/Fault;
- Verified Core has no known false proofs;
- Checker and stable backend support sets agree;
- MCDD semantic traces are equivalent across backends;
- Component IR, Native ABI 1, and Wire Schema 1 are frozen;
- Typed handle, allocator, callback, and async cancellation lifecycles are closed;
- Rust safe SDK and TypeScript GUI SDK pass end-to-end MCDD;
- ABI/schema/version/static Protocol handshake and compatibility matrix pass;
- External fact revision, Flow generation, and GUI projection have no dual-authority conflict;
- Any promoted stable effect/capability subset has consistent resolved summaries in checker, backends, Protocol, FFI, and verifier;
- Migration tool and guide are complete;
- All P0 blockers are zero.

---

## 10. Non-goals

*[source: devdocs/pre-0.1/00-core-goals.md §7, README.md §非目标]*

1.0 does not pursue:

- Putting all language constructs into Z3;
- Using auto-Fault completion for all unwritten business edges;
- Using auto reset/recover to replace business recovery design;
- Simultaneously stabilizing multiple borrow, error, or state models;
- Sacrificing core semantics for historical experimental syntax compatibility;
- Proving language maturity with more keywords;
- Rewriting in Mimi what Rust, TypeScript, Python, etc., do better with mature ecosystems.

This document does not define:

- Final per-token EBNF (see `docs/syntax-reference.md`);
- Standard library API freeze;
- Component ABI per-byte layout appendix;
- Specific parser or codegen implementation steps;
- Source compatibility policy;
- Using Z3 to cover complete Mimi language;
- Using auto-recovery to replace business recovery design;
- Reimplementing in Mimi what other languages already do better with mature ecosystems.

---

## 11. Normative Appendix: Small-Step Semantics

The operational meaning of the kernel's evaluation order, checked-integer
traps, and the linear-resource exactly-once discipline is pinned by the
normative appendix:

- [`docs/spec/small-step-semantics.md`](spec/small-step-semantics.md)
  (`mimi-small-step-1`): grammar, machine-integer arithmetic with trap
  semantics (SD-7), deterministic small-step rules and evaluation contexts,
  and the linear resource ledger invariant.

This appendix is normative; where the prose above is ambiguous about
evaluation order or trap behavior, the small-step rules govern.

## Change Principle

- RC allows concentrated destructive convergence, but must provide clear diagnostics and mechanical migration paths.
- Prohibit long-term retention of two safe syntaxes expressing the same concept.
- Prohibit parser accepting, checker passing, and backend then handling with warning, no-op, 0/null sentinel, or error degradation.
- Prohibit mixing implementation progress or version status into this document.
- Each stable semantic must have observable equivalence evidence in interpreter and native backends, or clearly be pure static/ghost semantics.

---

_This specification evolves with Mimi implementation. Once an item is marked RC-frozen, changes must record migration impact and re-pass relevant gates._
