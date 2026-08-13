# Mimi Error Codes Reference

> **Semantic authority**: `docs/language-spec.md` §4 (Error Model).
> Error code values remain valid; semantic definitions (Result/Fault/PeerFault/exit layering) defer to the specification.
> Diagnostic **output format contract** (dense single-line shape, required fields, invariants): `docs/diagnostics.md`.
> Sourced from `src/diagnostic/codes.rs`.

## Retirement Policy

**错误码一旦分配永不复用。** 废止码标注 `retired` + 原因，保留在文档中供历史参考。

| Code | Status | Reason |
|------|--------|--------|
| E0240 | retired | superseded by E0253 (where constraint violated) |
| E0241 | retired | superseded by E0254 (effect not available) |
| E0255 | retired | duplicate of E0235 (function does not return on all paths) |
| E0419 | retired | ADR-002 (v0.34.15) inverted the rejection — multi-target states MAY differ in payload layout; runtime dispatches by state tag (tagged-state-union ABI, v0.34.16) |
| E0701 | retired | duplicate of E0722 (unsupported expression in codegen) |

## Code Ranges

| Range | Category |
|-------|----------|
| E0001–E0099 | Lexical errors |
| E0100–E0199 | Syntax errors |
| E0200–E0299 | Type errors |
| E0300–E0399 | Ownership/borrow errors |
| E0400–E0499 | Semantic errors |
| E0500–E0599 | Contract/intention errors |
| E0600–E0699 | Warnings (legacy E06xx) |
| W001–W012  | Warnings (preferred W0xx) |
| W0400–W0499 | Flow warnings |
| E0700–E0722 | Codegen errors |
| E0741–E0742 | FFI errors |
| E0750–E0751 | File/resource errors |
| E0800+      | Runtime errors |
| ZONK-LEAK-001 | Type inference leak detector |

## Error Codes

| Code | Description |
|------|-------------|
| E0001 | unterminated string literal |
| E0002 | unterminated f-string |
| E0003 | illegal character |
| E0004 | unexpected end of file |
| E0005 | invalid integer literal |
| E0006 | invalid float literal |
| E0007 | tab indentation not allowed |
| E0100 | expected token |
| E0101 | expected ';' |
| E0102 | unexpected token at top level |
| E0103 | unexpected token in pattern |
| E0104 | unexpected token in expression |
| E0105 | unexpected token in statement |
| E0106 | missing '{' for block |
| E0107 | missing '}' to close block |
| E0108 | missing '(' for function call |
| E0109 | missing ')' to close call |
| E0110 | missing '->' for return type |
| E0111 | missing ':' for type annotation |
| E0112 | missing identifier |
| E0113 | missing string literal |
| E0114 | missing '=>' for match arm |
| E0115 | missing ',' between elements |
| E0116 | missing 'in' in for loop |
| E0117 | missing 'else' block |
| E0118 | unterminated interpolation in f-string |
| E0119 | '...' placeholder not allowed in production mode |
| E0120 | unexpected '$' character |
| E0200 | type mismatch |
| E0201 | cannot negate type |
| E0202 | cannot apply operator to types |
| E0203 | cannot apply '!' to type |
| E0204 | cannot dereference type |
| E0205 | if condition must be bool |
| E0206 | while condition must be bool |
| E0207 | return type mismatch |
| E0208 | cannot assign to immutable variable |
| E0209 | cannot assign type to variable of different type |
| E0210 | function argument count mismatch |
| E0211 | argument type mismatch |
| E0212 | for loop requires a List |
| E0213 | match expression must have at least one arm |
| E0214 | match arm body type mismatch |
| E0215 | match is not exhaustive |
| E0216 | match guard must be bool |
| E0217 | index must be integer |
| E0218 | cannot index type |
| E0219 | field access requires record type |
| E0220 | type has no field |
| E0221 | type has no method |
| E0222 | method call requires named type |
| E0223 | callee must be function name |
| E0224 | cannot apply '?' to type |
| E0225 | pattern type does not match subject |
| E0226 | constructor undefined |
| E0227 | variant takes no arguments |
| E0228 | variant argument count mismatch |
| E0229 | list element type mismatch |
| E0230 | comprehension guard must be bool |
| E0231 | type not allowed in this context (e.g., FFI passport type in non-extern) |
| E0232 | list element type mismatch |
| E0233 | cannot assign through non-mutable reference |
| E0234 | missing return value |
| E0235 | function does not return on all paths |
| E0236 | unreachable statement after return |
| E0237 | division by zero literal |
| E0238 | modulo by zero literal |
| E0239 | turbofish type argument count mismatch |
| E0240 | where constraint violated |
| E0241 | effect not available in scope |
| E0242 | builtin function error |
| E0243 | index out of bounds |
| E0244 | cannot index non-tuple type |
| E0245 | await requires Future type |
| E0246 | type has no variant |
| E0247 | record field type mismatch |
| E0248 | missing field in record literal |
| E0249 | name is not a record type |
| E0250 | comprehension requires list |
| E0251 | pattern mismatch |
| E0252 | missing method in trait impl |
| E0253 | where constraint violated |
| E0254 | retired | effect not available — abolished with the `with` effect clause (0.34.18c, §4.2 ruling); code never reused |
| E0255 | function does not return on all paths (deprecated: use E0235) |
| E0256 | linear capability not consumed |
| E0257 | function argument count mismatch |
| E0258 | shared binding type mismatch |
| E0259 | non-expr assignment target |
| E0300 | cannot borrow as mutable because already immutably borrowed |
| E0301 | cannot borrow as mutable because already mutably borrowed |
| E0302 | cannot borrow as immutable because already mutably borrowed |
| E0303 | capability must be consumed before end of scope |
| E0304 | capability already consumed |
| E0305 | retired | cannot capture local_shared in parasteps — local_shared removed (0.35.39, four-state shared → two-state); code never reused |
| E0306 | arena escape: reference to arena memory cannot outlive the arena block |
| E0400 | undefined variable |
| E0401 | undefined function |
| E0402 | duplicate definition |
| E0403 | variable shadows outer variable |
| E0404 | break outside of loop |
| E0405 | continue outside of loop |
| E0406 | undefined trait |
| E0407 | undefined type |
| E0408 | missing method in trait impl |
| E0409 | type alias cycle |
| E0410 | cannot infer record type without explicit type name |
| E0411 | weak requires a shared value |
| E0412 | protocol must be flat (no nested protocol-state payloads) |
| E0413 | unknown session type name |
| E0414 | session protocol order violation |
| E0415 | view/mutate borrow violation |
| E0416 | transition under pinned FFI anchor |
| E0417 | mutate parameter reassign (realloc/swap banned) |
| E0418 | subflow cannot be conservatively projected to flat protocol |
| E0419 | retired — ADR-002 (v0.34.15): incompatible payload layouts no longer rejected (see retired-code table) |
| E0420 | multi-target transition result must be exhaustively matched |
| E0421 | flow state forgery: non-root state constructed outside transition |
| E0422 | RETIRED (§3-诊断卫生 2026-08-07): flow-state unqualified-name shadowing migrated to W0402 (warnings must not wear E codes) |
| E0423 | flow state use-after-transition (linear generation) |
| E0424 | ? in transition body without fails E declaration |
| E0425 | session endpoint with non-end residual leaves scope |
| E0426 | session endpoint use-after-alias (linear consumption) |
| E0427 | linear type cannot be shared/borrowed |
| E0428 | ? on extern C call: FFI failures are Faults, not Rejected |
| E0429 | linear resource consumed before fallible operation |
| E0430 | from_json requires concrete type argument |
| E0431 | type escape hatch (`_`/Infer) leaked past its top-level inference boundary |
| E0432 | linear type (Cap/SessionChan/Flow state) passed as generic argument |
| E0433 | fails E combined with a multi-target transition — not yet supported (fail-closed) |
| E0434 | mutate argument is not a valid place (must be Ident or single-level Ident.field) |
| E0435 | two mutate arguments alias the same place (exclusive borrow violation) |
| E0436 | generic parameter name shadows a builtin type name |
| E0437 | trait method call on a bounded generic parameter (monomorphization deferred to 1.x; Clone is the supported exception) |
| E0438 | generic type argument count mismatch (split from E0231, §3-诊断卫生 2026-08-07) |
| E0439 | verification engine divergence: resolved and flow_ast engines disagree on a function verdict; fail-closed to the weaker conclusion (ADR-008 §3, 0.34.44). Arithmetic properties (e.g. `ensures: result == x * x`) commonly trigger this — the flow engine models integers as unbounded while the resolved engine applies i32/i64 checked semantics. Add explicit bounds (e.g. `requires: x <= 46340`) to remove the divergence |
| E0500 | cannot modify $-locked fragment |
| E0501 | strict mode: contract modifications not allowed |
| E0502 | contract on function with shared parameter is not verifiable by Z3 |
| E0600 | variable shadows outer variable |
| E0601 | unused variable |
| E0602 | unused import |
| E0700 | codegen internal error |
| E0702 | unsupported statement in codegen |
| E0706 | type not found in codegen |
| E0707 | field access on non-record type |
| E0708 | method not found on type |
| E0709 | builtin function error |
| E0710 | extern function not declared |
| E0712 | codegen internal error (json builtin) |
| E0713 | LLVM IR generation error |
| E0721 | unsupported binary operator |
| E0722 | unsupported expression in codegen |
| E0741 | FFI wrapper error |
| E0742 | value is not callable |
| E0750 | requires libc or I/O error |
| E0751 | assertion failed |
| E0800 | generic runtime error |
| E0801 | division by zero at runtime |
| E0802 | integer overflow at runtime |
| E0803 | index out of bounds at runtime |
| E0804 | wrong argument count at runtime |
| E0805 | non-exhaustive match at runtime |
| E0806 | concurrent lock error |
| E0807 | arena escape at runtime |
| E0808 | contract violation at runtime |
| E0809 | field not found at runtime |
| E0810 | runtime I/O error |
| E0811 | builtin function runtime error |
| E0812 | runtime type mismatch |
| E0813 | floating-point error |
| E0814 | slice out of bounds at runtime |

## Warning Codes (W0xx)

| Code | Description |
|------|-------------|
| W001 | standalone desc/rule has no implementation |
| W002 | locked fragment ($/$$) with no implementation body |
| W003 | `...` placeholder residual in .mimi files |
| W004 | function naming convention (snake_case) |
| W005 | shared variable written by multiple parallel steps in parasteps |
| W006 | unused variable |
| W007 | redundant parentheses |
| W008 | `== true` / `== false` anti-pattern |
| W009 | recursion depth hint |
| W010 | unused import |
| W011 | progressive Typestate migration (script mode → explicit flow) |
| W012 | type escape hatch (`_` or `Any`) bypasses static checks |

## Flow Warning Codes (W04xx)

| Code | Description |
|------|-------------|
| W0400 | flow state unreachable (no transition targets to it) |
| W0401 | flow state has no outgoing transitions (terminal state) |
| W0402 | flow state unqualified name shadowed by another flow (qualified name required) |

## Type Inference Leak Detector

| Code | Description |
|------|-------------|
| ZONK-LEAK-001 | unresolved type in function signature (TypeVar/Infer/ForAll leak) |