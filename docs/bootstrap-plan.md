# MimiSpec Bootstrap Plan (v0.29)

> Goal: compile the MimiSpec parser using Mimi itself, making Mimi self-hosting
> for its intent-description frontend.

---

## 1. Overview

Currently MimiSpec (`.mms`) parsing is handled by the external `mimispec`
crate. The v0.29 release will replace that dependency with a Mimi-implemented
parser that is compiled by Mimi. This document describes the steps, risks, and
rollback strategy.

---

## 2. Dependencies

| Dependency | Purpose | Already Available? |
|------------|---------|-------------------|
| Mimi compiler | Compile the MimiSpec parser source | Yes (v0.28.x) |
| Mimi lexer/parser | Tokenize/parse Mimi source | Yes (`lexer`, `parser`) |
| Mimi codegen | Generate object code | Yes (`codegen`) |
| Mimi runtime | Link symbols into parser binary | Yes (`runtime`) |
| MimiSpec grammar | Source-of-truth grammar (`gramma/`) | Yes |
| MimiSpec AST types | `mimispec` crate AST | Must be ported to Mimi `type` definitions |

---

## 3. Bootstrap Steps

### Phase 1 — Port MimiSpec AST to Mimi (v0.29.0-dev)

1. Translate the `mimispec` crate AST structs into Mimi `type` definitions.
   - Keep variants close to the Rust source to minimize porting risk.
2. Port `mimispec::parse()` to a Mimi function `parse_mimispec(source: string) -> ParseResult`.
3. Port `mimispec::render::render_file()` and LaTeX renderer if still needed.

### Phase 2 — Validate Parser Equivalence (v0.29.0)

1. Run the existing MimiSpec test corpus through both the Rust crate and the
   Mimi-implemented parser.
2. Compare ASTs (or rendered output) for equality.
3. Fix discrepancies before switching the default.

### Phase 3 — Integrate into `mimi` CLI

1. Replace the external `mimispec` dependency in `Cargo.toml` with the
   Mimi-compiled parser.
2. Update `mimi mms` to call the native parser.
3. Update `mms{}` block parsing in `src/parser/parse_stmt.rs` to call the
   native parser instead of spawning the external crate.

### Phase 4 — Self-Hosting Cycle

1. Use the Mimi-built parser to compile Mimi itself (or at least the parser
   portion).
2. Freeze the bootstrap entry version: document the exact compiler version
   required to build v0.29.0.

---

## 4. Rollback Strategy

- Keep the `mimispec` crate integration behind a Cargo feature (`external-mimispec`) for at least one release cycle.
- If the native parser cannot match the external parser's output, fall back to the external crate and defer the switch.
- Tag the last known good non-bootstrap version (`mimi-v0.28.x`) so bootstrap work can restart from a clean baseline.

---

## 5. Acceptance Criteria

- `cargo test` passes with the native parser.
- `mimi mms file.mms` produces identical output to the previous external-crate
  implementation on the test corpus.
- The Mimi-built parser binary can parse itself (or the parser source file)
  without crashing.
- `AGENTS.md` and this document are updated to reflect the new bootstrap
  status.

---

## 6. Open Questions

| Question | Owner | Target Resolution |
|----------|-------|-------------------|
| Should the MimiSpec AST be a separate `.mimi` stdlib module or inline? | Compiler team | v0.29.0-dev |
| How do we ship the parser source? Embedded string, file on disk, or compiled library? | Build team | v0.29.0-rc |
| Do we keep `mimispec` as a dev-dependency for corpus diff tests? | Test team | v0.29.0 |
