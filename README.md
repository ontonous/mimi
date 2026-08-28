<div align="center">

# Mimi Language

**A Flow-first, Typestate-Oriented system programming language**

[![Version](https://img.shields.io/badge/version-0.1.10--dev-blue.svg)](https://github.com/ontonous/mimi)
[![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-5400%2B-brightgreen.svg)](#)
[![Semantics](https://img.shields.io/badge/semantics-Pre--1.0-orange.svg)](#)
[![Clippy](https://img.shields.io/badge/clippy-zero%20warnings-orange.svg)](#)

Interpreter + LLVM 18 Codegen Dual Backend · Z3 Formal Verification · Sparse Flow State Machines · Linear Resources · Actor Model · Session Types · Component Boundary

---

</div>

## What is Mimi?

Mimi is a **Flow-first, Typestate-Oriented** system programming language. Its core insight: **replace lifetime annotations and `&mut self` with business-logic state machines (Flow)**. Every resource's lifecycle is bound to a business state — the compiler guarantees safety through state transitions, not borrow checking.

Mimi answers five questions from source code and types alone:

1. **What state** is this business object in?
2. **What events** are allowed in the current state?
3. **How do resources and ownership transfer** on state change?
4. **Is this failure** a local return, a state Fault, or a concurrent PeerFault?
5. **Which errors** can be rejected before the program runs?

```mimi
flow Order {
    state Pending
    state Paid
    state Shipped

    transition pay(Pending, payment: Payment) -> Paid { ... }
    transition ship(Paid, tracking: Tracking) -> Shipped { ... }
}
```

**Sparse, not dense.** You don't declare `pay(Paid)` or `ship(Pending)`. These combinations aren't missing matrix cells that auto-fill to Fault — **they don't exist at the type level**. Calling `ship` on a `Pending` order is a compile error, not a runtime fallback. Dynamic boundaries (network, FFI, deserialization) produce typed `UnexpectedEvent` errors, not fake business edges.

### Minimal Mental Model

| Construct | Sole Responsibility |
|-----------|-------------------|
| `func` | Stateless synchronous computation and composition |
| `flow` | Business state across time and its legal transitions |
| `actor` | Mailbox, scheduling, isolation, supervision; business state lives in Flow |
| `protocol` | Static state-topology projection of a Flow |
| `session` | Message ordering between two linear endpoints |
| `Result<T, E>` | Synchronous, recoverable failure |
| `Fault` | Flow invariant violation or unrecoverable state failure |
| `view / mutate / consume` | Read-only, in-place modification, and ownership transfer permissions |
| `requires / ensures` | Dynamically checked or statically proven contracts |
| `component / foreign` | Cross-language boundary with typed ownership, errors, and effects |

Mimi is the production compilation backend. MimiSpec (`.mms`) was removed in 0.1.8; write production Mimi directly (`.mimi`).

---

## Design Invariants

The Architecture Amendment (2026-07-25) established 10 design rulings after nine rounds of external blind review:

> **⚠ Design rulings, not a frozen API.** Mimi is an early experimental language far from 1.0: breaking changes are freely allowed, and "freeze" means **anchoring** development, not **locking** surface syntax. The rulings fix design *orientation* (sparse over dense, no nested Flow, no WAL) — their surface spelling is still breakable. The only long-term assets are the design ideas and the invariant suite (L1/L2/L3 + dual-backend equivalence). See CHANGELOG.md.

| # | Invariant | Meaning |
|---|-----------|---------|
| 1 | **Sparse is irreversible** | No dense mode. Undeclared `(state, event)` pairs are compile errors. |
| 2 | **No nested Flow** | Flow payload holds plain data only — handles, primitives, `shared`/`weak` refs. Never another Flow instance. |
| 3 | **No WAL** | The compiler does not generate transaction logs. Consistency is business logic. |
| 4 | **Recover = in-place reuse** | Recover preserves memory handles (XPU tensor reallocation is a disaster). Not transaction rollback. |
| 5 | **Generation by escape analysis** | No explicit syntax. Local handles: zero overhead. Cross-boundary handles: compiler packs generation automatically. |
| 6 | **View/Mutate is a 1.0 requirement** | `view`/`mutate` borrowing for pure function parameters. Not nice-to-have. |
| 7 | **Multi-target is a 1.0 requirement** | `transition parse(Pending) -> Connected \| Rejected` with nominal state tags. |
| 8 | **FFI failure = Fault** | Cross-FFI failure must enter Fault, never Rejected. Cannot undo C's side effects. |
| 9 | **No linear consumption before `?`** | Checker statically rejects consuming linear resources before a fallible operation. |
| 10 | **No synchronous pinned timeout** | Hanging C functions go to ForeignTask (async). No synchronous watchdog. |

> Full amendment: 13 clauses + 10 invariants. Supersedes the white paper where they conflict.

---

## Features

### Flow Core

| Feature | Status |
|---------|--------|
| `flow` / `state` / `transition` declarations, state payloads, transfer dispatch | ✅ |
| Sparse transition graph — undeclared `(state, event)` = compile error (`@sparse`) | ✅ |
| Typed Fault — per-flow `fault ErrorType` declaration | ✅ |
| `return S{}` terminal — unique transition terminal (become/stay removed per ADR-001) | ✅ |
| `fails E` rollback path — `?` returns `Err((source, error))`, source generation restored | ✅ |
| Reset / Recover system verbs (user-overridable) | ✅ |
| SystemTrace provenance (`last_state`, `unexpected_event`, snapshot) | ✅ |
| Progressive mode — script `main()` via implicit `flow Main { state Single }` shell (genuine semantic desugaring, spec §3.13) | ✅ |
| Multi-target transition (`-> A \| B` with state tags) | ✅ (stable tagged-union ABI, 0.34.15-16, ADR-002) |

### Linear Safety & Ownership

| Feature | Status |
|---------|--------|
| Flow state use-after-move rejection (E0423) | ✅ |
| Alias chain, closure capture, collection/tuple insertion rejection (E0427) | ✅ |
| CFG-level linearity — `is_linear()` for Flow states in dataflow analysis | ✅ |
| Session endpoint linearity — scope exit (E0425), use-after-alias (E0426) | ✅ |
| Shared/weak wrapping of linear resources rejected | ✅ |
| View/mutate borrowing (pure function parameter passing) | ✅ (0.34.13-14 closure + 0.34.25c place-grammar fail-closed, E0434/E0435) |
| Cross-turn exactly-once resource tracking | ✅ |
| Channel/Mutex/Atomic type-level linearity | 📋 (known limitation: builtin integer handles) |

### Concurrency

| Feature | Status |
|---------|--------|
| `actor Name runs FlowName` — Actor business state carried by Flow | ✅ (interp; codegen pending) |
| Session types: `session` / `dual` / `end`, compile-time residual checking | ✅ |
| Protocol interface abstraction (conservative projection subtyping) | ✅ |
| PeerFault cross-Actor propagation | ✅ |
| Mailbox backpressure auto-governance | ✅ |
| Spawn quota control (`@max_children(N)`) | ✅ |
| Polymorphic broadcast (`Vec<Protocol>`) | ✅ |

### Contracts & Verification

| Feature | Status |
|---------|--------|
| `requires:` / `ensures:` / `invariant:` in function bodies | ✅ |
| Z3 SMT solver integration (`mimi verify`) | ✅ |
| Runtime contract assertions (`mimi build --verify-contracts`) | ✅ |

### Dual Backend & Type System

| Feature | Status |
|---------|--------|
| Bytecode VM interpreter (sole interpreter since 0.1.3) + LLVM 18 codegen (native binary) — L1 equivalence tested | ✅ |
| Hindley-Milner type inference (undo trail + TypeScheme + zonk) | ✅ |
| Generics `<T: Bound>`, recursive types | ✅ |
| Enums / records / tuples, `match` exhaustiveness, `while let` | ✅ |
| `Option<T>` / `Result<T, E>` / `?` operator | ✅ |

### FFI, Comptime & Tooling

| Feature | Status |
|---------|--------|
| `extern "C"`, `repr(C)`, multi-language bindgen (C/C++/Rust/Go/Node.js/Java/Python) | ✅ |
| `comptime func` + `quote!` AST generation | ✅ |
| LSP: completion, hover, goto-definition, contract lens | ✅ |
| Package manager: `mimi.toml`, registry, git deps, dependency tree | ✅ |
| Cross-compilation: `--target` flag, shared library `.so` output | ✅ |
| Component IR + Native ABI + Wire Schema | ✅ (0.1.1 Phase C; SDK conformance green) |

---

## Quick Start

### Build

```bash
git clone https://github.com/ontonous/mimi
cd mimi
bash scripts/setup-llvm-wrapper.sh
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo build --release
```

### Hello, Flow

```mimi
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }

    transition inc(Zero) -> Positive {
        return Positive { count: self.count + 1 }
    }
    transition inc(Positive) -> Positive {
        return Positive { count: self.count + 1 }
    }
    transition reset(Positive) -> Zero {
        return Zero { count: 0 }
    }
}

func main() -> i32 {
    let s0 = Zero { count: 0 }
    let s1 = Counter::inc(s0)
    let s2 = Counter::inc(s1)
    println(s2.count)   // 2
    let s3 = Counter::reset(s2)
    println(s3.count)   // 0
    0
}
```

```bash
./target/release/mimi run counter.mimi
# => 2
# => 0
```

`Counter::inc(s0)` **consumes** `s0` — using `s0` after the transition is a compile error (E0423). Each transition produces a new generation of the state.

### Run Tests

```bash
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test
```

---

## Architecture

### CheckedProgram: The Semantic Hub

All backends (interpreter, LLVM codegen, Z3 verifier) consume a single source of truth: **CheckedProgram**. No backend re-parses AST or re-guesses types.

```
Source → Lexer → Parser → AST
  → HM Inference → Type Checker → CheckedProgram
    → Typed Resolved IR (canonical signatures, catalogs, materialized types)
    → CFG (per-callable control flow graph)
    → Resource Analysis (linear resource actions)
      ↓
  ┌───────────┼───────────┐
  Interpreter   Codegen     Verifier
  (from_checked) (compile_checked) (verify_checked)
```

**Iron rule**: backends cannot fall back to raw AST. Declaration layer (signatures, Flow transitions, Actor/Session, ownership, CFG) is fully installed from CheckedProgram; function bodies compile via per-function dispatch (resolved native emitter with an explicit, observable legacy arm). `CheckedProgram::raw_ast()` is crate-internal and limited to 3 permanent consumers (codegen pass 5 / interpreter / Z3 verifier).

### Dependency Chain

```
Span/Origin → HM → CFG/ownership → CheckedProgram/Resolved IR
  → Flow generation/turn → Actor/Session/resource → semantic trace
  → Verified Core
  → Component IR → Native ABI → Wire → Rust SDK / XPU FFI
```

### Core Abstractions

| Abstraction | Location | Role |
|-------------|----------|------|
| **CheckedProgram** | `src/core/checker/` | Single semantic hub: canonical signatures, catalogs, materialized types |
| **Typed Resolved IR** | `src/core/resolved/` | ResolvedFunction / ResolvedFlow / ResolvedTransition / ResolvedActor |
| **HM Unification** | `src/core/unification.rs` | Undo trail + TypeScheme + zonk; generic call fresh instantiation |
| **TypeFolder** | `src/core/type_folder.rs` | Binder-aware type folding (SurfaceTy / InferTy / ZonkedTy / BackendTy) |
| **CFG** | `src/core/cfg/` | Per-callable control flow graph, stable-ID CallableCfg |
| **Resource Analysis** | `src/core/ownership.rs` | Linear resource ledger (Introduce / Move / Drop / Return + borrow) with canonical action kinds |
| **AstNodeMeta** | `src/span.rs` | SourceId + Span + AstOrigin; NodeIdBuilder stable identity |

### Compiler Internal Flow Paradigm

The compiler itself is built on the Flow paradigm — each front-end module is a state machine with `fn transition(self, event) -> Self`. Five rules: no `&mut self`, no `Arc<Mutex<T>>`, no `unsafe`, no `transmute`/lifetime annotations, no bare `panic!`/`unwrap()`. Parser, Lexer, Loader, LSP, Verifier: strict Flow. Interpreter, Core Checker: relaxed Flow. Codegen, Runtime, FFI: non-Flow (LLVM API / C-style / text generation).

---

## Standard Library (24 modules)

| Module | Description |
|--------|-------------|
| `prelude` | identity, clamp, lerp, compose, pipe, fail, assert_msg |
| `io` | print_line, input_line, print_format, IoOps trait |
| `fs` | read, write, exists, read_lines, write_lines, file_size |
| `strings` | split, join, replace_all, capitalize, reverse, truncate, pad |
| `collections` | sort, map, filter, reduce, partition, group_by, chunks, dedup |
| `maps` | get, set, merge, pick, omit, has_key, from_list, filter_keys |
| `set` | contains, insert, remove, to_list, is_empty |
| `json` | to_json, from_json, get_int, get_bool, get_string, JsonExt trait |
| `net` | TCP socket, HTTP fetch/fetch_post, `Result<T, NetError>` |
| `csv` | parse_csv, serialize_csv |
| `crypto` | sha256, base64_encode/decode, hex_encode/decode |
| `template` | render_template |
| `time` / `datetime` | timestamp, sleep_ms, duration, days_from_now, time_since |
| `env` | get_var, cli_args, has_var, get_int, get_float |
| `mymath` | gcd, lcm, factorial, fibonacci, is_prime, is_power_of_two |
| `array` | fill, slice, rotate, binary_search |
| `iter` | range, zip, enumerate, take, drop, chain |
| `random` | random_int, random_float, random_range |
| `text` | slugify, indent, wrap |
| `result` | unwrap, map, map_err, and_then, or_else |
| `testing` | assert_eq_int, assert_true, assert_approx_eq_float |
| `effects` | Stdlib effect annotations (purity constraints for type checker) |
| `errors` | Typed error enums (FsError, JsonError, CollectionError) + From protocol |

Built-in regex (always available): `regex_match`, `regex_find`, `regex_replace`.

Built-in concurrency primitives (always available): `Mutex<T>`, `AtomicI32`/`AtomicI64`/`AtomicBool`, `Channel<T>`, `broadcast`.

---

## CLI Commands

| Command | Description |
|---------|-------------|
| `mimi check <path>` | Type-check with full error reporting |
| `mimi run <path>` | Run (interpret) with optional `--verify-contracts` / `--profile` / `--watch` |
| `mimi test <path>` | Run `test_*` functions with `--filter` and `--verbose` |
| `mimi build <path>` | Compile to native binary (LLVM). `--emit-ir`, `--shared`, `--target`, `--verify-contracts` |
| `mimi fmt <files>` | Format code (`--check` for CI) |
| `mimi lint <files>` | Static analysis (`--fail-on-warnings`) |
| `mimi verify <path>` | Z3 formal verification |
| `mimi lsp` | Start LSP server (stdin/stdout) |
| `mimi init [name]` | Initialize `mimi.toml` |
| `mimi add <name>` | Add dependency (`--version`, `--git`, `--path`) |
| `mimi remove <name>` | Remove dependency |
| `mimi install` | Install dependencies (`--frozen`, `--offline`) |
| `mimi update` | Update dependencies |
| `mimi list` | List dependencies |
| `mimi tree` | Show dependency tree |
| `mimi publish` | Publish to local registry |
| `mimi search <query>` | Search packages |
| `mimi doc <path>` | Generate documentation |
| `mimi promote <path>` | Upgrade legacy `.mms` sketch to `.mimi` (removed from user-facing surface after 0.1.8) |
| `mimi stats <path>` | Usage statistics |
| `mimi stat <path>` | Directory analysis |
| `mimi bindgen <path>` | Generate multi-language FFI bindings |
| `mimi abi core\|export\|validate\|hash\|diff\|check\|emit-c\|emit-rust\|emit-go\|emit-node\|emit-py\|emit-java\|emit-cpp` | Export/validate/generate/inspect Component `.mimiabi` JSON |
| `mimi wire encode\|decode\|validate-schema <file>` | Wrap/unwrap/validate Component Wire data |
| `mimi emit-*-bindings` | Language-specific FFI binding generation (C/C++/Rust/Go/Node.js/Java/Python) |

---

## Project Structure

```
mimi/
├── src/                        # Rust compiler (366 files, ~305k LOC)
│   ├── main.rs                 # CLI entry point (clap derive)
│   ├── lib.rs                  # Library entry point
│   ├── ast.rs                  # AST: FlowDef, StateDef, TransitionDef, ProtocolDef, ...
│   ├── span.rs                 # SourceId / Span / AstNodeMeta — stable node identity
│   ├── flow_matrix.rs          # Transition matrix + Fault injection
│   ├── session.rs              # Session type duality + sequencing check
│   ├── progressive.rs          # Script → implicit flow Main { state Single }
│   ├── trace.rs                # Canonical semantic trace (Transition / Fault / OwnershipTransfer)
│   ├── path_safety.rs          # Unified path validation
│   ├── source_scan.rs          # Shared SourceScanner (fmt/lint)
│   ├── parser/                 # Flow parser (strict Flow state machine)
│   ├── lexer/                  # Flow lexer (strict Flow state machine)
│   ├── core/                   # Type inference & checking → CheckedProgram
│   │   ├── checker/            # Type checker → CheckedProgram semantic hub
│   │   ├── resolved/           # Typed Resolved IR (canonical declarations)
│   │   ├── unification.rs      # HM unification (undo trail + TypeScheme)
│   │   ├── type_folder.rs      # Binder-aware type folding
│   │   ├── cfg/                # Per-callable control flow graph
│   │   ├── ownership.rs        # Linear resource analysis (canonical actions)
│   │   └── infer/              # HM type inference + contract derivation
│   ├── interp/                 # Bytecode VM (sole interpreter since 0.1.3)
│   │   └── bytecode/           # Bytecode compiler + VM + builtin registry
│   ├── codegen/                # LLVM 18 codegen (compile_checked)
│   │   └── builtins/           # Builtin function codegen (io, string, json, ...)
│   ├── verifier/               # Z3 contract verifier (verify_checked)
│   ├── ffi/                    # Multi-language binding generation (7 langs)
│   ├── lsp/                    # LSP server (strict Flow)
│   ├── loader/                 # Module loader (strict Flow)
│   ├── runtime/                # Rust runtime + actor mailbox + profiler
│   ├── fmt.rs                  # Code formatter
│   ├── lint.rs                 # Static linter
│   ├── main/                   # CLI subcommand implementations (24 commands)
│   ├── diagnostic/             # Error codes & formatting
│   └── tests/                  # 4500+ tests
├── std/                        # Standard library (24 modules)
├── examples/                   # Example programs (28)
├── demos/                      # Demo programs (23)
├── tests/real_world/           # MCDD real-world dual-backend suite (69 programs)
├── scripts/                    # Build & CI scripts
├── Cargo.toml
└── CHANGELOG.md
```

---

## Development

### Prerequisites

- **Rust** 1.75+
- **LLVM 18** (auto-configure via `scripts/setup-llvm-wrapper.sh`)
- **libffi** (FFI support)
- **Z3** (contract verification; handled by `cargo build`)

### Testing Tiers (IDD)

| Tier | Test | Meaning |
|------|------|---------|
| **L1** | `cargo test dual_` | Dual-backend equivalence (interp == codegen) |
| **L2** | `cargo test typecheck::` | Type system soundness (bad code rejected) |
| **L3** | `cargo test e2e_asan -- --ignored` | Memory safety (Valgrind/ASan/Miri) |

### Commands

```bash
# Full test suite
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test

# Dual-backend equivalence (L1)
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test dual_

# Type system soundness (L2)
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test "typecheck::"

# Real-world MCDD test suite
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test real_world

# Clippy (zero-warnings gate)
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo clippy --all-targets -- -D warnings

# Format
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo fmt
```

> **Test note**: after the test-performance work, the normal full gate is `ulimit -v 20000000 && LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test -- --test-threads=4` (about 42 seconds). Keep Z3 verification subsets single-threaded; extremely memory-constrained systems may fall back to `--test-threads=1`. Debug builds can still use about 12 GB RAM. See [AGENTS.md](AGENTS.md) for details.

---

## Status

**Current**: 0.1.10-dev. 0.1.7 shipped (2026-08-19): Wave-3 honest infrastructure closeout; 0.1.8 gates green (semantic honesty + identity purity); 0.1.9 shipped (2026-08-28): linear kinds + capabilities (cap true move + std, small-step semantics, E0439); 0.1.10-dev in progress (real-world pain-point repairs — integer-literal bidirectional coercion, Fault diagnostic, `state.method` desugar — and FFI component-symbol closure). Does not yet claim VM≡native.


### References & External Reviews

Curated key references and the nine-round external blind-review index are summarized in
CHANGELOG.md.


---


## Version History

### 1. Current Version
- **0.1.10-dev** (current): continues 0.1.9 (linear kinds + capabilities) into real-world
  pain-point repairs — integer-literal bidirectional coercion, Fault diagnostic,
  `state.method` desugar — and FFI component-symbol closure (M-004 `extern "C" const`
  export, M-001 export-prefix). See CHANGELOG.md.

### 2. Current Major Line (0.1.x)
- **0.1.0 → 0.1.8**: CheckedProgram semantic hub, Typed Resolved IR, HM unification, CFG/ownership,
  Bytecode VM as sole interpreter, full codegen migration, golden document + syntax freeze (0.1.4),
  Deep core closure (0.1.6), Wave-3 honest infrastructure closeout (0.1.7). Per-minor detail in
  CHANGELOG.md.

### 3. Pre-0.1 (v0.7 – v0.30)
- v0.7 (Z3 + FFI codegen) → v0.30 (hemostasis, 15 architecture debts cleared). 1863 commits,
  66 `mimi-v*` tags. Detailed history in CHANGELOG.md.

> Full changelog: [CHANGELOG.md](CHANGELOG.md).


---

## License

[Apache License 2.0](LICENSE)

Copyright © 2026 ontonous
