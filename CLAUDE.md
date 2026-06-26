# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build

```bash
# First-time: set up LLVM wrapper (avoids libpolly-18-dev dependency)
bash scripts/setup-llvm-wrapper.sh

# Compile
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo build

# Run tests (~13s, 2200+ tests)
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test --lib
```

## Test Commands

```bash
# All tests (lib)
cargo test --lib

# Dual-backend equivalence (L1 invariant — must all pass)
cargo test dual_

# Type system soundness (L2 invariant)
cargo test "typecheck::"

# FFI contract equivalence
cargo test ffi_

# Codegen E2E
cargo test codegen_e2e

# Ignored known gaps (must compile, may fail)
cargo test -- --ignored

# Miri UB detection
cargo miri test interp ffi
```

## Architecture

Mimi is a dual-backend (interpreter + LLVM codegen) language with Z3 contract verification. The source is organized as:

| Module | Purpose |
|--------|---------|
| `src/lexer/` | Tokenization |
| `src/parser/` | AST parsing |
| `src/ast.rs` | Core AST types (Expr, Stmt, Pattern, Type, Value) |
| `src/core/` | Type inference engine (unification, bidirectional checking) |
| `src/interp/` | Interpreter backend |
| `src/interp/eval/` | Expression/statement evaluation |
| `src/interp/eval/expr.rs` | `eval_call`, `eval_call_dispatch`, `eval_spawn`, `eval_await` |
| `src/interp/eval/stmt.rs` | `eval_let`, `eval_block`, `eval_arena_block`, `eval_parasteps` |
| `src/interp/call.rs` | Function/call resolution + `call_named` + constructor dispatch |
| `src/interp/ffi/` | FFI call support (callbacks, trampolines) |
| `src/interp/closure_utils.rs` | Free variable collection for closure capture |
| `src/interp/quote.rs` | Quoted AST evaluation (comptime) |
| `src/codegen/` | LLVM codegen backend |
| `src/verifier/` | Z3 contract verification |
| `src/runtime/` | C runtime (memory, threads, JSON, networking) |
| `src/tests/` | Integration test suite |

### Dual-Backend Invariant (IDD L1)

Every behavioral feature must produce identical results in both backends. The test macros in `src/tests/mod.rs`:

```rust
dual_assert!(program, "expected_output")   // both backends must match
dual_assert_ok!(program)                   // both run without error
```

`cargo test dual_` must be 100% green (excluding `#[ignore = "codegen gap: ..."]`).

### Key Interpreter Patterns

**Closure capture** (`closure_utils.rs`): `collect_free_vars` walks AST, accumulating bound names in `local_bound`. Arena/Block expressions properly create a local `local_bound` clone per nesting level. The `collect_stmt_free_vars` call takes `&mut local_bound` to accumulate bindings across statements.

**Parasteps** (`eval/stmt.rs`): `Stmt::Expr(Expr::Spawn(expr))` and `Stmt::Let { init: Some(Expr::Spawn(...)), .. }` both submit work to `pool::get_pool()`. Futures are collected in `futures: Vec` and awaited at block end. `spawn_bindings` maps names to futures for explicit `await` support.

**Arena escape** (`eval/stmt.rs:eval_arena_block`): After evaluating the arena block, scans outer scopes for `ArenaRef` values pointing to the dying arena. If found, returns `Err(ArenaEscape)`.

**FFI callbacks** (`ffi/callback.rs`): Thread-local `FFI_CALLBACK_CTX` stores `(interp_ptr, entries)` during synchronous C calls. `interp_ptr` is `*mut Interpreter<'static>` - cleared during the callback to prevent reentrancy UB (P1-7 fix). `apply_closure_ffi` is `&mut self` by design; the `*mut` cast and null-clear/re-restore pattern prevent nested mutable borrows.

### Codegen Key Files

| File | Purpose |
|------|---------|
| `codegen/mod.rs` | Main compilation driver, type layout, function codegen |
| `codegen/expr.rs` | Expression compilation |
| `codegen/builtins.rs` | Builtin function registration |

### Adding Builtin Functions

Four locations must be updated:
1. `codegen/builtins.rs` — register in `is_builtin_func` list
2. `codegen/mod.rs` — LLVM IR implementation
3. `src/interp/call.rs` — interpreter implementation
4. `src/core/infer_expr.rs` — type inference

### IDD Development Workflow

This project uses **Invariant-Driven Development** per `devdocs/idd-guide.md`:

1. **L1**: Write `dual_assert!` test before implementing any feature
2. Implement in interpreter → test passes
3. Implement in codegen → both backends pass
4. **L2**: Add type-checking negative tests (`check_source(bad) → Err`)
5. **L3**: Run Miri/Valgrind for UB
6. Commit with test name and invariant class in message

### Standard Library

Located in `std/*.mimi`. For regex, the interpreter uses Rust `regex` crate and codegen uses POSIX `regex.h`. For networking, both backends use `TCP_NODELAY`.

### Version Workflow

1. `Cargo.toml` version → `{next}-dev`
2. `CHANGELOG.md` → add `## [Unreleased] — {next}`
3. Commit with annotated tag
4. Implement features
5. **On release**: update CHANGELOG date, commit, tag `v{version}`
6. Update `AGENTS.md` §12 version status
