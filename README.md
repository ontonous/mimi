<div align="center">

# Mimi Language

**A Typestate-Oriented system programming language — Flow state machines, contract verification, and structured concurrency**

[![Version](https://img.shields.io/badge/version-0.1.0-blue.svg)](https://github.com/ontonous/mimi)
[![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-4063%20total-brightgreen.svg)](#)
[![Flow](https://img.shields.io/badge/flow-v0.29%20complete-orange.svg)](#)
[![Clippy](https://img.shields.io/badge/clippy-zero%20warnings-orange.svg)](#)

Interpreter + LLVM 18 Codegen Dual Backend · Z3 Formal Verification · Typestate-Oriented · Flow State Machines · Protocol/Session Types · Actor Model

---
</div>

---

## What is Mimi?

Mimi is a **Typestate-Oriented** system programming language. Its core insight: **replace lifetime annotations and `&mut self` with business-logic state machines (Flow)**. Every resource's lifecycle is bound to a business state — the compiler guarantees safety through state transitions, not borrow checking.

```mimi
flow Door {
    state Open   { opened_at: i64 }
    state Closed { locked: bool }

    transition open(Closed) -> Open {
        do { return Open { opened_at: timestamp() } }
    }
    transition close(Open { opened_at }) -> Closed {
        do { return Closed { locked: false } }
    }
    transition lock(Closed) -> Closed {
        do { return Closed { locked: true } }
    }
}
```

The compiler auto-completes the transition matrix — every undefined (state, event) pair gets `→ Fault`. No dangling states, no forgotten transitions.

---

## Features

| Category | Feature | Status |
|----------|---------|--------|
| **Flow** | `flow`/`state`/`transition` declarations, state payloads, transfer dispatch | ✅ v0.29.9 |
| **Flow** | Transition matrix auto-completion (+1 fallback to Fault) | ✅ v0.29.10 |
| **Flow** | Fault absorbing state + automatic resource drop | ✅ v0.29.11 |
| **Flow** | SystemTrace provenance (`last_state`, `unexpected_event`, snapshot) | ✅ v0.29.12 |
| **Flow** | Reset / Recover system verbs (Fault → root state, persistent keep) | ✅ v0.29.13 |
| **Flow** | Persistent payload + `@transactional` WAL rollback | ✅ v0.29.14 |
| **Flow** | `delegate view/mutate/consume` (3-level permission delegation) | ✅ v0.29.15 |
| **Flow** | `pinned { timeout }` FFI memory anchor | ✅ v0.29.16 |
| **Flow** | Subflow synchronous nesting (depth-first drop) | ✅ v0.29.17 |
| **Flow** | Protocol interface abstraction (conservative projection subtyping) | ✅ v0.29.18 |
| **Flow** | Session types: `session`/`dual`/`end`, compile-time linearity | ✅ v0.29.19 |
| **Flow** | PeerFault cross-Actor propagation | ✅ v0.29.20 |
| **Flow** | Mailbox backpressure auto-governance | ✅ v0.29.21 |
| **Flow** | Progressive typestate (script → implicit `flow Main { state Single }`) | ✅ v0.29.22 |
| **Flow** | `view`/`mutate` local borrowing (zero-overhead GEP pass) | ✅ v0.29.23 |
| **Flow** | Spawn quota control (`@max_children(N)`) | ✅ v0.29.24 |
| **Flow** | Polymorphic broadcast (`Vec<Protocol>`) | ✅ v0.29.25 |
| **Flow** | Protocol methods, session_pair, lifecycle | ✅ v0.29.27–31 |
| **Contract** | `requires:` / `ensures:` / `invariant:` in function bodies | ✅ |
| **Contract** | Z3 SMT solver integration (`mimi verify`) | ✅ |
| **Contract** | Runtime contract assertions (`mimi build --verify-contracts`) | ✅ |
| **Actor** | `actor` keyword, mutable fields, mailbox dispatch, worker thread | ✅ |
| **Dual Backend** | Interpreter (fast dev) + LLVM 18 codegen (native binary) | ✅ |
| **Generics** | `<T: Bound>` type parameters, recursive types | ✅ |
| **ADT** | Enums / records / tuples, `match` exhaustiveness, `while let` | ✅ |
| **Option/Result** | `Option<T>` / `Result<T, E>` / `?` operator | ✅ |
| **FFI** | `extern "C"`, `repr(C)`, multi-language bindgen (C/C++/Rust/Go/Node.js/Java/Python) | ✅ |
| **Comptime** | `comptime func` + `quote!` AST generation | ✅ |
| **LSP** | Completion, hover, goto-definition, contract lens | ✅ |
| **Package** | `mimi.toml` manifest, registry, git deps, dependency tree | ✅ |
| **Cross-compile** | `--target` flag, shared library `.so` output | ✅ |

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
        do { return Positive { count: self.count + 1 } }
    }
    transition inc(Positive) -> Positive {
        do { return Positive { count: self.count + 1 } }
    }
    transition reset(Positive) -> Zero {
        do { return Zero { count: 0 } }
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

### Run Tests

```bash
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test
```

---

## Standard Library (24 modules)

| Module | File | Description |
|--------|------|-------------|
| `prelude` | `prelude.mimi` | identity, clamp, lerp, compose, pipe, fail, assert_msg |
| `io` | `io.mimi` | print_line, input_line, print_format, IoOps trait |
| `fs` | `fs.mimi` | read, write, exists, read_lines, write_lines, file_size |
| `strings` | `strings.mimi` | split, join, replace_all, capitalize, reverse, truncate, pad |
| `collections` | `collections.mimi` | sort, map, filter, reduce, partition, group_by, chunks, dedup |
| `maps` | `maps.mimi` | get, set, merge, pick, omit, has_key, from_list, filter_keys |
| `set` | `set.mimi` | contains, insert, remove, to_list, is_empty |
| `json` | `json.mimi` | to_json, from_json, get_int, get_bool, get_string, JsonExt trait |
| `net` | `net.mimi` | TCP socket, HTTP fetch/fetch_post, NetError |
| `csv` | `csv.mimi` | parse_csv, serialize_csv |
| `crypto` | `crypto.mimi` | sha256, base64_encode/decode, hex_encode/decode |
| `template` | `template.mimi` | render_template |
| `regex` | (builtins) | regex_match, regex_find, regex_replace |
| `time` / `datetime` | `time.mimi` / `datetime.mimi` | timestamp, sleep_ms, duration, days_from_now, time_since |
| `env` | `env.mimi` | get_var, cli_args, has_var, get_int, get_float |
| `mymath` | `mymath.mimi` | gcd, lcm, factorial, fibonacci, is_prime, is_power_of_two |
| `array` | `array.mimi` | fill, slice, rotate, binary_search |
| `iter` | `iter.mimi` | range, zip, enumerate, take, drop, chain |
| `random` | `random.mimi` | random_int, random_float, random_range |
| `text` | `text.mimi` | slugify, indent, wrap |
| `result` | `result.mimi` | unwrap, map, map_err, and_then, or_else |
| `testing` | `testing.mimi` | assert_eq_int, assert_true, assert_approx_eq_float |

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
| `mimi promote <path>` | Upgrade `.mms` → `.mimi` |
| `mimi mms <files>` | Process MimiSpec files |
| `mimi stats <path>` | Usage statistics |
| `mimi stat <path>` | Directory analysis |
| `mimi bindgen <path>` | Generate multi-language FFI bindings |
| `mimi emit-c-headers` / `emit-py-bindings` / `emit-rust-bindings` / `emit-go-bindings` / `emit-node-bindings` / `emit-cpp-bindings` / `emit-java-bindings` | Language-specific FFI binding generation |

---

## Project Structure

```
mimi/
├── src/                       # Rust compiler (323 files, ~172k LOC)
│   ├── main.rs                # CLI entry point (clap derive)
│   ├── lib.rs                 # Library entry point
│   ├── ast.rs                 # AST: FlowDef, StateDef, TransitionDef, ProtocolDef, ...
│   ├── flow_matrix.rs         # Transition matrix + Fault auto-completion (+1 fallback)
│   ├── session.rs             # Session type duality + sequencing check
│   ├── progressive.rs         # Script → implicit flow Main { state Single }
│   ├── parser/                # Flow parser (strict Flow state machine)       ✅ v0.29.0
│   ├── lexer/                 # Flow lexer (strict Flow state machine)        ✅ v0.29.1
│   ├── core/                  # Type inference & checking (relaxed Flow)      ✅ v0.29.8
│   ├── interp/                # Interpreter (relaxed Flow)                    ✅ v0.29.6
│   ├── codegen/               # LLVM 18 codegen via inkwell
│   │   └── builtins/          # Builtin function codegen (io, string, json, ...)
│   ├── verifier/              # Z3 contract verifier (strict Flow)            ✅ v0.29.7
│   │   └── flow.rs            # Verifier as Flow state machine
│   ├── ffi/                   # Multi-language binding generation (7 langs)
│   ├── lsp/                   # LSP server (strict Flow)                     ✅ v0.29.5
│   ├── loader/                # Module loader (strict Flow)                   ✅ v0.29.4
│   ├── runtime/               # Rust runtime + actor mailbox + profiler
│   ├── fmt.rs                 # Code formatter
│   ├── lint.rs                # Static linter
│   ├── main/                  # CLI subcommand implementations
│   ├── diagnostic/            # Error codes & formatting
│   └── tests/                 # 3100+ tests across 96 modules
├── std/                       # Standard library (24 modules)
├── examples/                  # Example programs (28+)
├── demos/                     # Demo programs (23+)
├── devdocs/                   # Design docs: white paper, flow drafts, ADRs
├── scripts/                   # Build & CI scripts
├── Cargo.toml
└── CHANGELOG.md
```

---

## Architecture: Flow Paradigm

The compiler itself is built on the same Flow paradigm it compiles — each module is a state machine:

| Module | Flow Degree | Status |
|--------|-------------|--------|
| Parser | Strict Flow | ✅ v0.29.0 (454 LOC) |
| Lexer | Strict Flow | ✅ v0.29.1 (970 LOC) |
| Loader | Strict Flow | ✅ v0.29.4 |
| LSP | Strict Flow | ✅ v0.29.5 |
| Verifier | Strict Flow | ✅ v0.29.7 |
| Core Checker | Relaxed Flow | ✅ v0.29.8 |
| Interpreter | Relaxed Flow | ✅ v0.29.6 |
| Codegen | Non-Flow (LLVM API) | N/A |
| Runtime | Non-Flow (C-style unsafe) | N/A |
| FFI | Non-Flow (text generator) | N/A |

**Five rules** of the Flow paradigm:
1. No `&mut self` — use `fn transition(self, event) -> Self`
2. No `Arc<Mutex<T>>` — use `enum + transition`
3. No `unsafe` in Flow modules
4. No `transmute` or lifetime annotations
5. No bare `panic!`/`unwrap()`/`expect()` — return `Result<Self, Error>`

---

## Development

### Prerequisites

- **Rust** 1.75+
- **LLVM 18** (auto-configure via `scripts/setup-llvm-wrapper.sh`)
- **libffi** (FFI support)
- **Z3** (contract verification; handled by `cargo build`)

### Testing Tiers

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

> **Memory note**: `cargo test` in debug mode can use ~12 GB RAM. Use `ulimit -v 20000000` and `--test-threads=1` on memory-constrained systems. See [AGENTS.md](AGENTS.md) for details.

---

## Version History

| Version | Highlight |
|---------|-----------|
| **0.1.0** | **Baseline stability**: CheckedProgram semantic hub, semver switch, runtime/resolved architecture split, 4063 tests green, dual-backend equivalence |
| **v0.30.0** | **止血 (Hemostasis)**: Zero new features — architecture debt repair (sprintf→snprintf, path safety, malloc checks, values_equal, build_unreachable, fmt tokenization) |
| **v0.29.41** | White paper freeze: all 38 capabilities complete ✅ |
| **v0.29.37** | Actor lifecycle: SystemKill cascade + `spawn detached` |
| **v0.29.34** | Session dual-end runtime: send/recv/close push endpoints |
| **v0.29.32** | Pinned collaborative watchdog: `pinned { timeout }` |
| **v0.29.25** | Flow polymorphic broadcast, session_pair, mutate forwarding |
| **v0.29.18** | Protocol interface abstraction (conservative projection subtyping) |
| **v0.29.14** | Persistent payload + `@transactional` WAL rollback |
| **v0.29.9** | Flow language baseline: `state`/`transition` dual-backend |
| **v0.29.0–8** | Compiler internal Flow architecture replacement (Parser→Lexer→Loader→LSP→Interp→Verifier→Checker) |
| **v0.28.37** | Feature bugs zero — last v0.28 release |
| **v0.28.0** | Use-driven: 7-lang FFI, profiler, bindgen, package manager |
| **v0.27** | Safety audit: P0/P1/P2/P3 (arena, FFI, JSON, runtime) |
| **v0.24** | Structured concurrency state machine |
| **v0.20** | Future/Waker/Executor/poll codegen |
| **v0.15** | C runtime → Rust runtime rewrite |
| **v0.7** | Z3 verification + FFI codegen |

> Full changelog in [CHANGELOG.md](CHANGELOG.md).

---

## License

[Apache License 2.0](LICENSE)

Copyright © 2026 ontonous
