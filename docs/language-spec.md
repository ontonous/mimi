# Mimi Language Specification (1.0 Draft)

> **Authority**: This document is the single canonical specification entry point for Mimi 1.0.
> It is extracted from the Pre-1.0 design contracts in `devdocs/pre-1.0/` (00–08).
> All other documentation must defer to this file for semantic definitions.
>
> **Target status**: Normative sections are `stable` unless explicitly marked `experimental`, `reserved`, or `removed`. Current implementation maturity is non-normative and lives only in `docs/language-support.toml`.
>
> **Version**: v1.0-spec-draft (2026-07-17)

Normative requirements use stable IDs defined in `docs/language-requirements.toml`. Design rationale lives in `devdocs/pre-1.0/`; implementation structure and progress live in `docs/ast-appendix.md` and `docs/language-support.toml`. Parser acceptance and an existing implementation do not grant stable status.

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

*[source: devdocs/pre-1.0/00-core-goals.md §1–§3]*

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

*[source: devdocs/pre-1.0/00-core-goals.md §4]*

### 2.1 State Invariants `[stable]`

- Flow states have fully qualified nominal identity, e.g., `Order::Paid`.
- State values cannot be arbitrarily forged from outside the Flow.
- A Flow instance has exactly one current state at any moment.
- Transition consumes the old state; the old state cannot be used after transition.
- Self-loops also produce a new state generation; old aliases cannot be retained.
- Multi-target transitions must preserve runtime state tag. `[experimental]`
- Each transition turn must end with exactly one of: `become`, `stay`, typed `fault`, or rollback failure.
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

### 2.7 Effect and Capability Invariants `[experimental]`

- Effect describes what an operation may produce; Capability describes whether the caller is authorized to trigger it; the two cannot substitute for each other.
- The proposed minimum effect set includes: `pure`, `alloc`, `io`, `blocking`, `spawn`, `ffi`, `unsafe`; mutation is also constrained by `view/mutate/consume` and shared effect.
- Undeclared effects must not be silently expanded by function body, external import, or dynamic dispatch.
- Caller's effect must cover callee effect; `pure` can only call pure/total operations.
- Actor turn defaults to prohibiting unknown-duration `blocking`; FFI and callback blocking/reentrant/thread effect must enter Component IR.
- Capability is nominal, unforgeable, scope/audience-restrictable, revocable; holding a capability does not change the Flow's currently allowed event set.
- Effect inference can reduce boilerplate, but exported API, Protocol, Component Boundary, and proof artifact must have stable, explicit, comparable resolved effect summary.
- Higher-order effect polymorphism remains experimental if inference, subtyping, and backend closure cannot be completed.

The entire effect/capability surface remains experimental until the minimum effect lattice, caller-covers-callee rule, capability issuance/delegation/revocation semantics, and resolved summaries are frozen. At that point the closed minimum subset may be promoted independently; higher-order effect polymorphism remains experimental.

---

## 3. Flow-first Core Model

*[source: devdocs/pre-1.0/01-flow-first-model.md §2–§12]*

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

Terminal actions are exactly four kinds:

- `become Target { ... }`: commit new state;
- `stay { ... }`: commit same-named state with new generation;
- `fault FaultVariant(...)`: commit typed Fault;
- Rollback failure: no new state committed; caller regains original source generation and typed error.

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

### 3.7 Multi-target Transition `[experimental]`

Multi-target transition is only stable when preserving nominal state tag:

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

Multi-target transition is not part of the minimum 1.0 RC stable core. Stable single-target Flow semantics and typed dynamic boundary errors do not depend on it.

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

### 3.9 Protocol `[stable]`

Protocol is a static topology projection of a Flow, not an ordinary trait, nor a default runtime reflection object.

This stable commitment is limited to `StaticProtocolProjection`: checker-verified topology, statically generated language interfaces, stable Protocol identity, and version handshake. Type-erased `dyn Protocol`, runtime VTable dispatch, heterogeneous collections, and dynamic broadcast are `experimental` and must be independently feature-gated.

Stable Protocol describes:

- Visible states;
- Events allowed in each visible state;
- Input and output payloads;
- Permission/effect constraints;
- Fault exposure strategy.

Flow implementing Protocol: checker at least proves:

- Required states and business edges exist;
- Payload variance conforms to view/mutate/consume permissions;
- Implementation does not expand prohibited effects;
- Target state maintains Protocol's nominal identity mapping.

String-based `protocol_methods("Name")` is not part of the stable type-safe model. `[removed]`

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

Stable rules:

- Old endpoint invalid after operation;
- Endpoint must not implicitly convert to integer;
- Alias, fields, return values, and branch merge preserve residual;
- Cannot skip check when unable to track endpoint; must error;
- Non-`end` endpoint leaving scope must explicitly return, transfer, or error;
- Session runtime and checker use the same protocol ID.

Any Session program that cannot prove residual completeness must be rejected.

Minimum dual-end linear Session is a 1.0 core goal; any unclosed item blocks RC. Recursive protocols, dynamic participants, delegation, multiparty Session, and cross-version residual upgrade remain experimental.

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
Fault should be a Flow-specific, typed fault set, not a global catch-all record.

Conceptual example:

```mimi
fault OrderFault {
    Storage(StorageError)
    Peer(PeerFault)
    Timeout(Duration)
    UnexpectedEvent {
        state: StateId,
        event: EventId,
    }
    Panic(PanicPayload)
}
```

Recovery rules:

- Anticipated business failures prefer `Result`; do not automatically enter Fault;
- Invariant breakage, unrecoverable runtime errors, and explicitly absorbed panics can enter Fault;
- Recover must match specific Fault variant;
- Reset/recover does not auto-generate business implementations;
- Persistent resource must have explicit recovery strategy;
- Secondary Fault must be recorded or escalated; cannot be silently swallowed.

### 3.13 Progressive Mode `[stable]`

Simple scripts can use Mimi without first learning complete Flow, but implicit Main must be a genuine semantic desugaring.

```mimi
func main() {
    println("hello")
}
```

The compiler must put its real body into the implicit Flow's startup transition, not just insert a shell Flow and continue the traditional main path.

Rules:

- Pure synchronous, no persistent resources, no concurrency: script mode allowed;
- Once using Actor, spawn, Session, phased resources, or recover: require explicit Flow or provide applicable migration fix-it;
- CLI can display lowered Flow;
- Diagnostics always map back to user source positions.

---

## 4. Error Model and Debug Prevention

*[source: devdocs/pre-1.0/02-errors-and-debug-prevention.md §2–§12]*

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

### 4.3 Typed Fault `[stable]`
Fault represents a Flow's inability to maintain its state invariants, not a catch-all for all errors.

Each Flow should declare or derive its own fault set:

```mimi
fault OrderFault {
    Storage(StorageError)
    Peer(PeerFault)
    Timeout(Duration)
    UnexpectedEvent { state: StateId, event: EventId }
    Panic(PanicPayload)
}
```

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
- Fault variant and business payload;
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
1.0 converges to scope guards:

- `defer`: execute cleanup whether scope exits normally or abnormally;
- `defer failure`: execute compensation only when scope exits with `Err`, Fault absorption, or panic;
- Transition rollback failure is a failure exit; `become`/`stay` is not;
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

Recover uses explicit Fault variant and recoverable data to construct target state.

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

*[source: devdocs/pre-1.0/03-verified-core.md §1–§14]*

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
- Comptime, quote, and generated code;
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

#### `math` `[removed]`

General `math { Expr... }` is removed from the RC stable set unless a pure ghost AST and Verified Core rules are established.

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

*[source: devdocs/pre-1.0/04-language-coherence.md §2–§14]*

### 6.1 Functions: `func` and `fn` `[stable]`

- `func name(...)`: named function definition;
- `fn(...) { ... }`: anonymous closure;
- `fn(T) -> U`: function value/function pointer type;
- `extern "C" fn(T) -> U`: FFI function pointer.

Memory rule: named functions use `func`; functions-as-values use `fn`.

#### Convergence `[removed]`

- `func(T) -> U` as function type: **removed**; migrate to `fn(T) -> U`;
- `extern "C" func(...)` type spelling: **removed**;
- Parser must not accept both spellings in the same context.

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

### 6.3 Ownership: Flow payload and shared/weak `[stable]`

- Flow payload defaults to exclusive, linearly transferred by transition;
- `shared`/`local_shared` indicates explicit shared object graph;
- `weak`/`weak_local` indicates non-owning reference to shared object;
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
- Ordinary Actor arbitrary mutable business field and mutating method: **removed** from stable set;
- Actor helper only allows stateless computation;
- Mailbox method call migrates to typed Flow event;
- Actor runtime holds unique Flow instance.

### 6.5 Abstraction: trait and Protocol `[stable]`

- `trait`: stateless value interface;
- `protocol`: Flow's state topology, event, and permission projection;
- `session`: communication endpoint message ordering.

Three cannot assume each other's responsibilities.

#### Protocol Convergence `[removed]`

- Protocol state payload uses same record schema as Flow state;
- Permissions written as view/mutate/consume constraints;
- String-based runtime `protocol_methods("Name")`: **removed**;
- Typed compile-time reflection can be provided in `comptime`;
- Dynamic `dyn Protocol` remains experimental until typed VTable, event/result ABI, and dual-backend consistency.

### 6.6 Session `[stable]` / `[experimental]`

Session enters stable set only after:

- `session_pair::<P>()` returns `SessionChan<P>` and `SessionChan<dual P>`;
- Endpoint is a linear value;
- send/recv/close advance residual;
- Alias, fields, return, closures, and branch merge preserve tracking;
- Untracked reports error;
- No bare `List<i64>` or integer handle user API;
- Interpreter/native runtime behavior consistent.

Recursive protocols, dynamic participants, delegation, multiparty Session, and cross-version residual upgrade remain experimental.

### 6.7 Transition body and `do` `[removed]`

Remove the semantically empty `do` wrapper. Transition's `{ ... }` is itself the implementation body:

```mimi
transition ship(Paid) -> Shipped
    fails TrackingError
{
    let tracking = allocate_tracking()?
    become Shipped { tracking }
}
```

If body uses `?`, signature must declare rollback failure error type; failure returns source generation.

### 6.8 Contracts: requires, ensures, invariant, math `[stable]`
#### Function-exclusive structure

Contracts are exclusive fields of function/transition definitions:

```mimi
func withdraw(balance: i64, amount: i64) -> i64
    requires amount >= 0
    requires amount <= balance
    ensures result == balance - amount
{
    balance - amount
}
```

#### `invariant`

- Flow state invariant belongs to state/Flow declaration;
- Loop invariant belongs to loop header;
- Function invariant if no independent meaning: not retained;
- Runtime and static verifier check timing must be explicit and dual-backend consistent.

#### `math` `[removed]`

General `math { Expr... }` removed from RC stable set.

### 6.9 MimiSpec Meta-syntax: `desc`, `rule`, `mms` `[removed]`

- `.mms` retains MimiSpec's `desc`, `rule`, and intent structures;
- Production `.mimi` `desc`/`rule` statement: **removed** from stable syntax;
- `mms {}` no longer an executing `Stmt` affecting checker, contract detection, or verifier: **removed**;
- If needing to associate MimiSpec, use documentation metadata, trivia attachment, or external mapping;
- Unrecognized metadata must warning/error; must not pretend verified.

### 6.10 Comptime and Quote `[stable]` / `[experimental]`

#### `comptime`

- Only calls comptime or explicitly pure functions;
- Prohibits I/O, spawn, Actor, FFI, shared mutation;
- Return value must serialize to compile-time constant;
- Effect/purity enforced by checker;
- Evaluation failure is hard error;
- Runtime not generating comptime symbol is normal; no misleading warning.

#### `quote`

Quote/AST generation remains experimental until:

- Can faithfully represent all allowed generated AST;
- Does not silently filter contracts, Flow statement, or metadata;
- Generated result re-passes complete parse/check/lower;
- Span/hygiene/phase isolation explicit;
- Generated code and verifier boundary explicit.

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
- Flat typed Protocol;
- Minimum dual-end typed Session;
- Typed Fault/PeerFault;
- Function-exclusive contracts;
- Restricted pure comptime;
- Effect/capability is not a stable target in this profile; its proposed minimum model remains experimental under `EFFECT-CAP-001`.

#### Experimental

- Multi-target (until tagged union lowering);
- Dynamic Protocol/VTable/broadcast;
- Quote/AST generation;
- In-process FFI signal recovery and forced thread termination;
- Compiler auto-synthesized recover (explicit typed reset/recover is stable);
- Heterogeneous Actor collection;
- Explicit low-level references outside runtime/FFI;
- Higher-order effect polymorphism.

#### Removed / Migrated

- Semantic-less `do`;
- `func(...) -> T` function type;
- `extern "C" func(...)` type;
- `.mimi` `desc`/`rule` statement;
- Executing AST `mms` statement;
- String-based Protocol reflection;
- Shared wrapper auto-stripping;
- Failure variant name heuristics;
- `?` process exit;
- User-visible bare Session `i64` handle;
- Actor arbitrary mutable business fields;
- Unknown attribute silent ignore;
- `|>` as transition target separator;
- `math { Expr... }` general statement.

---

## 7. Component Boundary, Native ABI, and Wire Schema

*[source: devdocs/pre-1.0/07-first-class-ffi.md §1–§21]*

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
| `ffi slice<T>` | `{ptr,len}` read-only view |
| `ffi slice_mut<T>` | `{ptr,len}` exclusive view |
| `ffi str` | UTF-8 `{ptr,len}` view |
| `ffi owned_str` | Owned UTF-8 string with allocator/destructor |
| `ffi c_str` | Explicit NUL-terminated C string |
| `ffi buffer<T>` | owned `{ptr,len,cap,allocator}` buffer |

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

*[source: devdocs/pre-1.0/06-multilanguage-strategy.md §1–§12]*

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

*[source: devdocs/pre-1.0/05-rc-migration-and-gates.md §4, §12]*

### 9.1 P0 RC Blockers

#### Flow and Actor

- Flow state freely forgeable or copyable: **blocks RC**;
- Old state usable after transition: **blocks RC**;
- Codegen transition resolution has first-candidate fallback: **blocks RC**;
- Actor mutable business field bypassing Flow: **blocks RC**;
- Undeclared business combination default-injected as transition: **blocks RC**;
- Dynamic event without typed boundary error: **blocks RC**;
- A build that accepts experimental multi-target while losing the state tag or silently selecting the first target violates the specification and **blocks RC**; the minimum RC may instead disable and reject the experimental syntax;
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

*[source: devdocs/pre-1.0/00-core-goals.md §7, README.md §非目标]*

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

## Change Principle

- RC allows concentrated destructive convergence, but must provide clear diagnostics and mechanical migration paths.
- Prohibit long-term retention of two safe syntaxes expressing the same concept.
- Prohibit parser accepting, checker passing, and backend then handling with warning, no-op, 0/null sentinel, or error degradation.
- Prohibit mixing implementation progress or version status into this document.
- Each stable semantic must have observable equivalence evidence in interpreter and native backends, or clearly be pure static/ghost semantics.

---

_This specification evolves with Mimi implementation. Once an item is marked RC-frozen, changes must record migration impact and re-pass relevant gates._
