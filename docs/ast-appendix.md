# Mimi AST Appendix: Normalized and Injected AST

> **Status**: Non-normative implementation appendix to `docs/language-spec.md`.
> **Purpose**: Document current AST transformations and gaps between parser output and backend consumption.
> **Sources**: `src/ast.rs`, `src/flow_matrix.rs`, `src/progressive.rs`, `src/parser/mod.rs`
>
> Snapshot: v0.30.0 (2026-07-17)

This file cannot define language stability or override the specification. Target IR requirements are indexed by `docs/language-requirements.toml`; current maturity is reported by `docs/language-support.toml`.

---

## 1. Parser Output AST

The parser produces a `File` containing top-level `Item` entries. This is the raw user-authored AST.

### 1.1 Top-level Items (`ast.rs:21`)

| Item Variant | AST Node | Source Parser |
|---|---|---|
| `Func(FuncDef)` | Named function | `top_level.rs` |
| `Module(ModuleDef)` | Module declaration | `top_level.rs` |
| `Type(TypeDef)` | Type alias / struct / enum / record | `top_level.rs` |
| `Actor(ActorDef)` | Actor with fields and methods | `top_level.rs:436` |
| `Cap(CapDef)` | Capability declaration | `top_level.rs` |
| `Trait(TraitDef)` | Trait definition | `top_level.rs` |
| `Impl(ImplDef)` | Trait implementation | `top_level.rs` |
| `ExternBlock(ExternBlock)` | External C declarations | `top_level.rs` |
| `Const` | Compile-time constant | `top_level.rs` |
| `Flow(FlowDef)` | Flow state machine | `top_level.rs:706` |
| `Protocol(ProtocolDef)` | Protocol topology | `top_level.rs` |
| `Session(SessionDef)` | Session type | `top_level.rs:1190` |

### 1.2 Statement Variants (`ast.rs:270`)

24 statement variants. Key variants for normalization tracking:

| Stmt Variant | Current representation | Notes |
|---|---|---|
| `Let` | present | Variable binding |
| `Return` / `Break` / `Continue` | present | Control flow |
| `If` / `While` / `WhileLet` / `Loop` / `For` | present | Control structures |
| `Block` | present | Block statement |
| `Match` (via `Expr::Match`) | present | Pattern matching |
| `Assign` | present | Assignment |
| `Requires` / `Ensures` / `Invariant` | legacy-present | Currently body statements; canonical target is `LANG-CONTRACT-001` |
| `Math` | legacy-present | Canonical target is `SYNTAX-REMOVED-001` |
| `Desc` / `Rule` / `MmsBlock` | legacy-present | Canonical target is `SYNTAX-REMOVED-001` |
| `Do` | legacy-present | Canonical target is `SYNTAX-REMOVED-001` |
| `OnFailure` | legacy-present | Canonical target is `SYNTAX-REMOVED-001` |
| `SharedLet` | legacy-present | Must lower to unified `let` + constructor |
| `Delegate` | present | Subflow delegation |
| `Pinned` | present | FFI memory pinning |
| `Parasteps` | present | Parallel steps |
| `Arena` / `Unsafe` / `Drop` / `Alloc` | present | Low-level constructs |
| `Func(FuncDef)` | present | Nested function definition |

### 1.3 Expression Variants (`ast.rs:365`)

31 expression variants including: `Literal`, `Ident`, `Binary`, `Unary`, `Call`, `Field`, `Index`, `Tuple`, `List`, `Comprehension`, `Match`, `Record`, `Block`, `Try` (`?`), `Spawn`, `Await`, `Quote`, `Comptime`, `Lambda`, `Old`, `SliceExpr`, `Range`, `Turbofish`, `Cast`, etc.

---

## 2. Normalized AST (Checker-Injected Transformations)

After parsing, the compiler applies several AST transformations before checker/codegen consume the AST. These are **system-injected** nodes, not user-authored.

### 2.1 Progressive Main Desugaring

**Current maturity**: `partial`.

**Location**: `src/progressive.rs:17` `fn apply_progressive_typestate`

**Trigger**: File has no user `flow` but has top-level `main` function.

**Action**:
1. Set `file.implicit_single = true`
2. Insert `Item::Flow(make_implicit_main_flow())` at position 0

**Injected Flow** (`progressive.rs:43` `make_implicit_main_flow`):
```mimi
flow Main {
    state Single
    transition run(Single) -> Single
}
```

**Call sites**: `src/parser/mod.rs:167` (parse_file), `:257` (parse_str), `src/parser/flow.rs:465-466, :476`

**Design intent**: Main body is placed into the implicit Flow's startup transition, not a shell. Currently the lowering is not fully genuine semantic desugaring.

### 2.2 Transfer Matrix Auto-Completion (+1 Fault Fallback)

**Current maturity**: `partial`; this unconditional behavior conflicts with stable target semantics.

**Location**: `src/flow_matrix.rs:83` `fn expand_flow_with_shapes`

**Action**:
1. `ensure_fault_state(flow)` (`:85`) — ensures `Fault` state exists in the Flow
2. Iterates `states × events` matrix
3. For undefined `(state, event)` pairs (`:117`): constructs `TransitionDef { is_fallback: true, to_states: vec!["Fault"] }`
4. `flow.transitions.extend(fallbacks)` (`:129`)

**Injected nodes**: Fallback transitions with `is_fallback: true`, targeting `Fault` state.

**Design intent** (pre-1.0): Stable mode prohibits auto-completing undeclared combinations into business transitions. Automatic boundary fallback is only permitted for prototype/REPL, explicitly declared open dynamic boundaries, and test fault injection. It is currently applied unconditionally.

### 2.3 System Verb Injection (reset / recover)

**Current maturity**: `partial`; automatic business recovery conflicts with target semantics.

**Location**: `src/flow_matrix.rs:458` `fn inject_system_verbs`

**Action**: When user has not defined `reset` or `recover` from `Fault`:
- Injects `transition reset(Fault) -> root_state` (`:474`, `is_fallback: true`)
- Injects `transition recover(Fault) -> root_state` (`:494`, `is_fallback: true`, with `keep` strategy based on `persistent_fields`)

**Called by**: `expand_flow_with_shapes` at `:132`

**Design intent**: Reset/recover should be business transitions, not auto-generated defaults. User-defined body takes priority. Auto-injection still occurs.

### 2.4 PeerFault Default Cascade Injection

**Current maturity**: `partial`; no declarative supervision policy exists.

**Location**: `src/flow_matrix.rs:272` `fn inject_peer_fault_verbs`

**Action**: For each non-Fault state without a user-defined `peer_fault` transition:
- Injects `peer_fault(State) -> Fault` (`is_fallback: true`)
- Injects `peer_fault(Fault) -> Fault` self-loop no-op (`:306-318`)

**Runtime propagation**: `src/interp/value.rs:920` `fn notify_peer_faults` — iterates `peer_links`, sends `peer_fault` message to each un-faulted peer's mailbox. Called by `short_circuit_mailbox` (`:977`) before marking faulted.

**Design intent**: PeerFault should not default to unconditionally converting local Flow to Fault. Receiver should choose via supervision strategy.

### 2.5 FFI Pinned Transition Injection

**Current maturity**: `partial`; this records current lowering, not stable support.

**Location**: `src/flow_matrix.rs:417, :435, :454`

**Action**: Injects `enter_ffi`, `exit_ffi`, and `ffi_crash` transitions with `is_ffi_pinned: true`.

**Injected transitions**:
- `enter_ffi(Active) -> FFI_Pinned`
- `exit_ffi(FFI_Pinned) -> Active`
- `ffi_crash(FFI_Pinned) -> Fault` (with `is_fallback: true`)

---

## 3. System-Injected vs User-Authored AST Markers

The compiler distinguishes system-injected nodes from user-authored nodes using two boolean fields on `TransitionDef` (`ast.rs:663, :666`):

### 3.1 `is_fallback: bool` (`ast.rs:663`)

**Definition**: `pub is_fallback: bool` — "True when this transition was injected by transfer-matrix auto-completion"

**Set to `true` at**:
- `flow_matrix.rs:124` — matrix fallback transitions
- `flow_matrix.rs:301, :316` — peer_fault injections
- `flow_matrix.rs:453` — ffi_crash transition
- `flow_matrix.rs:481, :501` — reset/recover injections

**Consumed at**:
- `src/interp/eval.rs:642` — interpreter transition dispatch
- `src/codegen/expr/call/method.rs:364` — codegen method dispatch
- `src/core/checker/items.rs:1358, :1627` — checker item processing
- `src/core/check_stmt.rs:1606` — statement checking

### 3.2 `is_ffi_pinned: bool` (`ast.rs:666`)

**Definition**: `pub is_ffi_pinned: bool` — marks FFI_Pinned `enter_ffi`/`exit_ffi`/`ffi_crash` transitions

**Set to `true` at**: `flow_matrix.rs:417, :435, :454`

### 3.3 User-Authored Transitions

User-written transitions have both `is_fallback: false` and `is_ffi_pinned: false`.

### 3.4 Missing Markers

The pre-1.0 design requires:
- System injected nodes distinguished by AST flags (04 §13) — **partially implemented** (only `is_fallback` and `is_ffi_pinned`)
- User Flow transition must have body while Protocol transition signatures may be body-less — **not enforced consistently; do not claim complete**
- `is_system_verb` field — **not implemented**; system verbs (reset/recover/peer_fault) use `is_fallback: true` instead

---

## 4. Required Next Layer: Typed Resolved IR

*[source: devdocs/pre-1.0/05-rc-migration-and-gates.md §2 Phase 2]*

The normative requirement `TOOL-RESOLUTION-001` requires a typed resolved IR layer between checker and backends. The current implementation is partial: declaration catalogs and finalized function signatures exist, while body-level calls, conversions, effects, Session residuals, and general resource facts are not yet fully typed. The target semantics remain authoritative in the language specification.

### 4.1 Current State

- Function/Flow/state/transition declarations use canonical, module-qualified `NodeId`/`FlowId`/`StateId`/`TransitionId`; process-local dense numeric IDs are not yet materialized.
- Checker unification produces fail-closed `ZonkedTy` function signatures, persisted by `CheckedProgram` under canonical function `NodeId`; body expression types and checked conversions remain incomplete.
- Effect names and declared Session bodies are persisted in resolved directories, but effect capability resolution and per-local Session residuals remain checker-internal.
- Capability Introduce/Move/Drop/Return summaries are persisted per callable; general place/resource/loan identity awaits the CFG/ownership milestones.

### 4.2 Target State

The resolved IR should include:
- Resolved function/transition ID (numeric, unique)
- Fully qualified Flow/State ID
- Parameter permission/effect (resolved)
- Resolved effect set and required capability
- Result/Option propagation target
- Session residual
- Resource move/drop
- Backend capability requirement

Checker becomes the sole resolver of stable semantics. Interp/codegen must not re-guess types, transitions, or failure variants.

---

## 5. Checker Resolution Summary

### 5.1 Current Resolution Points

| Resolution | Location | Output |
|---|---|---|
| Type inference | `src/core/infer/` | `Type` (may contain `Infer`/`TypeVar`) |
| Flow transition dispatch | `src/codegen/compile.rs:267` | String name `{flow}__{trans}__from_{state}` |
| Effect checking | `src/core/checker/items.rs:377` | `has_effect` boolean |
| Capability checking | `src/core/checker/vars.rs:175` | `is_cap_var` boolean |
| Session residual | `src/core/checker.rs:93` | `HashMap<String, Residual>` |
| Borrow mode | `src/core/checker/borrow.rs` | `ParamBorrow::{View, Mutate}` |

### 5.2 Gaps to Target

| Gap | Impact | Priority |
|---|---|---|
| No resolved transition ID | Codegen may re-guess; non-unique | P0 |
| No resolved effect set | Effect inference is incomplete | P1 |
| No IR-level Session residual | Residual lost between checker and backend | P1 |
| No IR-level resource tracking | No exactly-once proof | P0 |
| No backend capability gate | Unsupported semantics deferred to codegen warning | P0 |

---

_This appendix tracks AST transformations. When new injections or normalizations are added, update this document._
