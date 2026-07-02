<div align="center">

# 🧬 Mimi Language

**A system programming language with contract verification, structured concurrency, and linear capabilities**

[![Version](https://img.shields.io/badge/version-0.28.8-blue.svg)](https://github.com/ontonous/mimi)
[![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-2388%20passed%20%7C%200%20failed-brightgreen.svg)](#)
[![Clippy](https://img.shields.io/badge/clippy-zero%20warnings-orange.svg)](#)

MimiSpec Production Compiler Backend · Z3 Formal Verification · LLVM Native Compilation · Interpreter + Codegen Dual Backend

---

> **⚠️ Development Status**
> Mimi is under **active pre-stable development**. Version `0.x` means the language, APIs, standard library, and CLI are all subject to change. **Not yet recommended for production use.**
> Early adopters are warmly welcome to test, report issues, and join the discussion — every issue and every suggestion matters.

</div>

---

## Table of Contents

- [Get Involved](#get-involved)
- [Features Overview](#features-overview)
- [Quick Start](#quick-start)
- [Examples](#examples)
- [Standard Library](#standard-library)
- [CLI Commands](#cli-commands)
- [Project Structure](#project-structure)
- [Version History](#version-history)
- [Development](#development)
- [Contributing](#contributing)
- [License](#license)

---

## Get Involved

Mimi is evolving fast and we'd love your feedback. Whether you're a programming language enthusiast, a systems software developer, or just curious about contract-driven development, there's a place for you here.

### Current Status

- **Core Language**: Type system, borrow checking, concurrency model — foundational components are in place and continuously refined.
- **Standard Library**: 21 modules covering common scenarios; interfaces may still evolve.
- **Toolchain**: Compiler, LSP, and package manager are all functional, though not yet at 1.0 stability.
- **Verification & Codegen**: LLVM native compilation and Z3 contract verification are improving steadily; some advanced verification scenarios may still be incomplete.

### How to Get Involved

| Method | Path |
|---|---|
| **Report Bugs** | Open an [Issue](https://github.com/ontos-hpc/mimi/issues) with reproduction steps, platform info, and a minimal reproducer. |
| **Feature Requests** | Describe your use case and expected behavior via an Issue. |
| **Improve Docs** | Syntax reference, standard library comments, example programs — any change that makes Mimi easier to learn is welcome. |
| **Contribute Code** | Read [CONTRIBUTING.md](CONTRIBUTING.md) and start with a `good first issue`. |
| **Write Examples & Tutorials** | Share your Mimi programs to help others understand the language. |
| **Join Discussions** | GitHub Issues & Discussions — ask questions, share experiences, or talk about design trade-offs. |

### When Will It Be Stable?

There is no fixed release date yet. The team iterates based on internal roadmaps and community feedback, with milestones recorded in [CHANGELOG.md](CHANGELOG.md). If you depend on a specific feature or want API freeze, let us know in an Issue — use cases directly drive priorities.

> 💡 Even starring the repo or telling a friend you're trying Mimi makes a difference.

---

## Features Overview

Mimi is the production compiler backend for the **MimiSpec** intent-description language, differentiated by **contract verification, structured concurrency, and linear capabilities**.

| Feature | Description |
|---|---|
| **Contract Verification** | `requires`/`ensures` pre/post conditions + Z3 formal verification + runtime assertions |
| **Structured Concurrency** | `parasteps` parallelism + `spawn`/`await` + `on failure` LIFO compensation |
| **Linear Capabilities** | `cap` type-level resource tracking + `Allocator` custom allocators |
| **Dual Backend** | Interpreter (rapid dev) + LLVM 18 codegen (native compilation) |
| **Borrow Checking** | `&T`/`&mut T`, path-sensitive, arena escape detection, reborrowing |
| **Reference Counting** | `shared`/`local_shared`/`weak` ownership model |
| **Generics & Lifetimes** | `<T: Clone>` bounds, lifetime elision, recursive types |
| **Option / Result** | `Option<T>` full path + `Result<T, E>` + `?` operator |
| **ADT & Pattern Matching** | Enums/records/tuples, `match` exhaustiveness, `while let` |
| **FFI** | `extern "C"`, `repr(C)` struct-by-value, callbacks, multi-language binding generation (C/C++/Rust/Go/Node.js/Java/Python/TypeScript) via `mimi bindgen` |
| **async** | `async fn` → Future state machine + Executor cooperative scheduling |
| **LSP** | Language server: completion, hover, goto-definition, contract lens |
| **Package Management** | `mimi.toml` + registry + git dependencies + dependency tree |
| **Standard Library** | 23 modules: io, fs, net, json, csv, crypto (SHA-256/Base64), regex, template, paths, and more |
| **MimiSpec Integration** | `.mms` parsing, `mms{}` placeholders, rule consistency checking |
| **Compile Targets** | Native x86_64, cross-compilation to Windows, shared library `.so` |

---

## Quick Start

### Build from Source

```bash
# Clone
git clone https://github.com/ontos-hpc/mimi
cd mimi

# Setup LLVM 18
bash scripts/setup-llvm-wrapper.sh

# Build
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo build --release

# Verify
./target/release/mimi --version
```

### Hello World

```mimi
func greet(name: string) -> string {
    "Hello, " + name + "!"
}

func main() -> i32 {
    println(greet("World"));
    0
}
```

```bash
./target/release/mimi run hello.mimi
# => Hello, World!
```

### Run Tests

```bash
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test
# 2383 passed, 0 failed, 21 ignored
```

---

## Examples

### Function with Contracts

```mimi
pub func divide(a: i32, b: i32) -> i32 {
    requires: b != 0          // divisor must not be zero
    ensures:  result == a / b // result must be correct
    a / b
}
```

Enable Z3 formal verification with `mimi build --verify-contracts`.

### ADT & Pattern Matching

```mimi
type Tree<T> {
    Leaf(T)
    Node(Tree<T>, Tree<T>)
}

func depth<T>(t: Tree<T>) -> i32 {
    match t {
        Leaf(_) => 1,
        Node(l, r) => 1 + max(depth(l), depth(r)),
    }
}
```

### Concurrency & Compensation

```mimi
func process() -> Result<i32, string> {
    let data = fetch_data()?;
    on failure { cleanup(data) }

    let result = compute(data)?;
    on failure { revert(result) }

    Ok(result)
}
```

### FFI Calls

```mimi
extern "C" {
    func strlen(s: string) -> i64;
    func puts(s: string) -> i32;
}

func main() {
    let len = strlen("Mimi");
    puts("Hello from Mimi FFI!");
}
```

> More examples in [`examples/`](examples/) (28 `.mimi` programs).

---

## Standard Library

| Module | File | Description |
|---|---|---|
| `io` | `io.mimi` | I/O: `print_line`, `input_line` |
| `fs` | `fs.mimi` | Filesystem: `read`, `write`, `exists` |
| `strings` | `strings.mimi` | Strings: `split`, `join`, `replace_all` |
| `collections` | `collections.mimi` | Collections: `sort`, `map`, `filter`, `reduce` |
| `maps` | `maps.mimi` | Map ops: `get`, `set`, `merge`, `pick` |
| `set` | `set.mimi` | Set ops: `contains`, `insert`, `remove` |
| `json` | `json.mimi` | JSON: `to_json`, `from_json`, typed deserialization |
| `net` | `net.mimi` | Networking: TCP socket, HTTP fetch |
| `csv` | `csv.mimi` | CSV parsing and serialization |
| `crypto` | `crypto.mimi` | Crypto: SHA256, base64, hex |
| `template` | `template.mimi` | String template rendering |
| `regex` | (builtins) | Regex match/find/replace |
| `time` / `datetime` | `time.mimi` / `datetime.mimi` | Timestamp / datetime utilities |
| `env` | `env.mimi` | Env vars / CLI arguments |
| `mymath` | `mymath.mimi` | Math: gcd, lcm, is_prime |
| `random` | `random.mimi` | Random number utilities |
| `text` | `text.mimi` | Text: slugify, indent, wrap |
| `result` | `result.mimi` | Result combinators |
| `prelude` | `prelude.mimi` | Utilities: clamp, pipe, compose |
| `testing` | `testing.mimi` | Test assertions |

---

## CLI Commands

| Command | Description |
|---|---|
| `mimi check <file>` | Type check |
| `mimi run <file>` | Run (type check + interpret) |
| `mimi build <file>` | Compile to native binary |
| `mimi build --verify-contracts` | Build with contract assertions |
| `mimi test <file>` | Run `test_*` functions |
| `mimi fmt <files>` | Format code |
| `mimi lint <files>` | Lint |
| `mimi verify <file>` | Z3 formal verification |
| `mimi lsp` | Start LSP server |
| `mimi init <name>` | Initialize project |
| `mimi add <name>` | Add dependency |
| `mimi remove <name>` | Remove dependency |
| `mimi install` | Install dependencies |
| `mimi update` | Update dependencies |
| `mimi list` | List dependencies |
| `mimi tree` | Show dependency tree |
| `mimi publish` | Publish to local registry |
| `mimi search <query>` | Search packages |
| `mimi doc <file>` | Generate docs |
| `mimi promote <file>` | `.mms` → `.mimi` promotion |
| `mimi mms <files>` | Process MimiSpec files |
| `mimi stats <file>` | Usage statistics (function-level call counts) |
| `mimi emit-c-headers <file>` | Emit C headers |
| `mimi emit-cpp-bindings <file>` | Emit C++ RAII bindings |
| `mimi emit-rust-bindings <file>` | Emit Rust FFI bindings |
| `mimi emit-go-bindings <file>` | Emit Go CGO bindings |
| `mimi emit-node-bindings <file>` | Emit Node.js N-API bindings + TypeScript `.d.ts` |
| `mimi emit-java-bindings <file>` | Emit Java JNI bindings |
| `mimi emit-py-bindings <file>` | Emit Python pybind11 bindings |
| `mimi bindgen <file> -o <dir>` | Generate all language bindings at once |
| `mimi stat [path]` | Directory statistics (files, dirs, extensions) |
| `mimi run --profile <file>` | Run with function-level profiling |

---

## Project Structure

```
mimi/
├── src/                   # Rust source code
│   ├── main.rs            # CLI entry point
│   ├── lib.rs             # Library entry point
│   ├── ast.rs             # AST definitions
│   ├── parser/            # Parser
│   ├── lexer/             # Lexer
│   ├── core/              # Type checking & inference
│   ├── interp/            # Interpreter backend
│   ├── codegen/           # LLVM codegen backend
│   ├── verifier/          # Z3 formal verifier
│   ├── ffi/               # FFI system (C/C++/Rust/Go/Node.js/Java/Python bindings)
│   ├── lsp/               # LSP server
│   ├── contracts.rs       # Contract extraction
│   ├── runtime/           # Rust runtime + profiler
│   ├── fmt.rs             # Formatter
│   ├── lint.rs            # Linter
│   ├── manifest.rs        # Package manifest
│   ├── loader.rs          # Module loader
│   └── tests/             # Test suite
├── std/                   # Standard library (21 modules)
├── examples/              # Examples (29 programs)
├── docs/                  # Documentation
│   ├── adr/               # Architecture Decision Records
│   ├── syntax-reference.md
│   └── ...
├── scripts/               # Build/test scripts
├── benches/               # Benchmarks
├── CHANGELOG.md           # Full changelog
├── CONTRIBUTING.md        # Contributing guide
├── CODE_OF_CONDUCT.md     # Code of conduct
├── SECURITY.md            # Security policy
└── LICENSE                # Apache-2.0
```

---

## Version History

| Version | Highlight |
|---|---|
| **v0.28.8** | Codegen quality refactor + helper unit tests + lexer/parse dual-backend tests, clippy clean |
| **v0.28.2** | Usability: Record/Any type annotations, codegen map ops, `const`, `as` cast, `desc{}`/`rule{}` blocks |
| **v0.28.1** | mimi-kv embedded store, map type inference fix, dual-backend map tests |
| **v0.28** 🚀 | Use-driven evolution: dir/path/crypto builtins, multi-language FFI (7 langs), profiler, `mimi stat`/`mimi bindgen`, 2 design defect fixes |
| **v0.27** 🔨 | Audit fixes: P0/P1/P2/P3 safety & correctness (arena, FFI, JSON, runtime) |
| **v0.26** | Type unification engine + bidirectional inference + FFI P0/P1 security fixes |
| **v0.25** | Type system refactor: TypeId Arena + Checker fixes + Newtype/ADT codegen |
| **v0.24** | Structured concurrency state machine + runtime/FFI/codegen fixes |
| **v0.23** 🔨 | Z3 deep fix + internal audit |
| **v0.22** | Language completion: Option/nested generics/loop/pipe/LSP |
| **v0.21** | Clippy zero + codegen gap closure + docs |
| **v0.20** | Structured concurrency: Future/Waker/Executor/poll codegen |
| **v0.19** | Path-sensitive borrow + reborrow + conditional return |
| **v0.18** | Generic bounds + lifetime elision + built-in traits |
| **v0.17** | GEP safety abstraction + 62 unsafe removals |
| **v0.16** | FFI fix + effect system + pattern exhaustiveness |
| **v0.15** | C runtime → Rust runtime rewrite |
| **v0.14** | Diagnostics: error codes + Z3 debug output |
| **v0.13** | Verification coverage: closure/spawn/await/string |
| **v0.12** | FFI zero-copy + crypto/CSV/template stdlib |
| **v0.11** | Windows target + net stdlib |
| **v0.10** | Backend alignment + CI/CD |
| **v0.9** | Safety: arena escape/race detection |
| **v0.8** | Package management + docs pipeline |
| **v0.7** | Z3 verification + FFI codegen |

> Full changelog in [CHANGELOG.md](CHANGELOG.md).

---

## Development

### Prerequisites

- **Rust** 1.75+
- **LLVM 18** (auto-configure via `scripts/setup-llvm-wrapper.sh`)
- **libffi** (FFI support)
- **Z3** (contract verification; handled by `cargo build`)

### Command Quick Reference

```bash
# Run all tests
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test

# L1 dual-backend equivalence
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test dual_

# L2 type system soundness
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test typecheck::

# Clippy (zero-warnings gate)
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo clippy --deny warnings

# Format
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo fmt

# Benchmarks
cargo bench
```

### Development Principles

| Tier | Test Category | Meaning |
|---|---|---|
| **L1** | Dual-backend equivalence | Interpreter and codegen produce identical results |
| **L2** | Type system soundness | Invalid code is correctly rejected |
| **L3** | Memory safety | Zero Valgrind/ASan warnings |

---

## Contributing

We warmly welcome all forms of contribution:

- **Try it & give feedback**: Build the project, run the examples, and file Issues for anything confusing or broken.
- **Documentation & translation**: Fix typos, add comments, translate sections — help Mimi reach more developers.
- **Write tests & examples**: Contribute programs under `examples/` or write tutorials for existing features.
- **Code contributions**: See [CONTRIBUTING.md](CONTRIBUTING.md) for coding standards and the submission process. Start with a `good first issue`.
- **Design discussions**: Participate in Issues on language features, API design, error messages — your use case is the best design input.
- **Community building**: Answer questions, share the project on social media, and help build a welcoming community.

> All participants must adhere to the [Code of Conduct](CODE_OF_CONDUCT.md). Report security issues privately via the [Security Policy](SECURITY.md).

---

## License

[Apache License 2.0](LICENSE)

Copyright © 2026 ontonous
