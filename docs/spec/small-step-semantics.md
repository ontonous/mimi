# Small-Step Semantics 1（规范性附录）

> Normative appendix to `language-spec.md`.
> Profile: `mimi-small-step-1`. This is a **normative** model of the core
> kernel's evaluation order and linear-resource discipline. It does not
> replace the prose spec; it pins the operational meaning of evaluation
> order, checked-integer traps, and exactly-once linear movement.

## 1. Scope

The appendix covers the **kernel** `K` — a strict, sequentially evaluated
subset of Mimi sufficient to ground the invariants:

- values: `i32`, `i64`, `bool`, `string`, `unit`;
- checked integer arithmetic (`+ - * / %`, `-x`) with **trap** semantics on
  overflow / zero divisor / `MIN ÷ -1` (SD-7);
- comparisons and short-circuit `&&` / `||`;
- `let` bindings, statement sequences, blocks, tail `return`;
- total function calls (no recursion termination obligation here);
- linear values and the resource ledger.

Sessions, actors, Flow transitions, I/O and native calls are out of kernel
scope; their operational models live in their own appendices.

## 2. Grammar

```text
v ::= i32 | i64 | bool | string | unit          (values; machine integers)
e ::= v
    | x                                        (identifier)
    | e ⊕ e                                    (⊕ ∈ {+,-,*,/,%})
    | e ⊙ e                                    (⊙ ∈ {==,!=,<,>,<=,>=})
    | e && e | e || e | !e
    | let x = e in e
    | if e { e } else { e }
    | match e { p => e, ... }                  (single-binding patterns)
    | f(e1,...,en)                             (total function application)
    | return e
b ::= e | x = e ; b | let x = e ; b | return e ; b
```

Machine integers carry their width: `i32(x)` and `i64(x)` with `x ∈ ℤ`. The
denotation of an arithmetic operator on machine integers is defined below;
raw ℤ is never a runtime value.

## 3. Configurations

A configuration is `⟨b; ρ; L⟩`:

- `b` — the block under evaluation;
- `ρ` — an environment mapping identifiers to values;
- `L` — the **linear resource ledger**: a multiset of identifiers currently
  holding a live linear value (capability, SystemToken, MutexGuard, flow
  state, session endpoint, or a container/tuple that nests one).

An initial configuration is `⟨f(a1..an); ρ0; ∅⟩` for a call `f(a1..an)` where
`ρ0` binds the parameters (linear parameters enter `L`).

The **result** of a terminating evaluation is either `⟨v⟩` (a value), `⇓`
(divergence), or `↯` (trap — a checked-integer or division-definedness
violation). Traps are `E0802` (integer overflow, incl. `MIN ÷ -1`) or `E0801`
(zero divisor);
a trap aborts the step — it is **not** a recoverable fault.

## 4. Machine Integer Arithmetic

Let `w = 32` or `64`, with `min(w) = -2^(w-1)`, `max(w) = 2^(w-1)-1`.
Define `result_w(x ⊕ y)`:

```text
+ : x + y           if min ≤ x+y ≤ max,  else ↯
- : x - y           if min ≤ x-y ≤ max,  else ↯
* : x * y           if min ≤ x*y ≤ max,  else ↯
/ : x / y           if y ≠ 0 and not (x = min ∧ y = -1), else ↯
% : x % y           if y ≠ 0 and not (x = min ∧ y = -1), else ↯
```

`-x` (unary negate): `min ≤ -x ≤ max` else `↯`. The result is re-wrapped to
the same width `w`. There is **no** implicit widening or wrapping in the
kernel: every operator is checked and every violation is a trap.

## 5. Small-Step Rules

Write `→` for one evaluation step. `E[·]` ranges over evaluation contexts
(defined below); the rule application is the congruence:

```text
   b → b'
───────────────────   (Cong)
 E[b] → E[b']
```

### 5.1 Values and environments

```text
x ∈ dom(ρ)
──────────────  (Var)
⟨E[x]; ρ; L⟩ → ⟨E[ρ(x)]; ρ; L⟩
```

### 5.2 Binary arithmetic (checked)

```text
result_w(v1 ⊕ v2) = w
──────────────────────────  (Arith)
⟨E[v1 ⊕ v2]; ρ; L⟩ → ⟨E[w]; ρ; L⟩
```

```text
result_w(v1 ⊕ v2) = ↯
──────────────────────────  (Arith-Trap)
⟨E[v1 ⊕ v2]; ρ; L⟩ → ↯
```

### 5.3 Comparisons and logic

```text
v1 ⊙ v2 = b        (b ∈ {true,false}, on ℤ ordering)
──────────────────────────  (Cmp)
⟨E[v1 ⊙ v2]; ρ; L⟩ → ⟨E[b]; ρ; L⟩

⟨E[true  && e2]; ρ; L⟩ → ⟨E[e2]; ρ; L⟩   (And-True)
⟨E[false && e2]; ρ; L⟩ → ⟨E[false]; ρ; L⟩ (And-False)
⟨E[true  || e2]; ρ; L⟩ → ⟨E[true];  ρ; L⟩ (Or-True)
⟨E[false || e2]; ρ; L⟩ → ⟨E[e2]; ρ; L⟩   (Or-False)
⟨E[!true];  ρ; L⟩ → ⟨E[false]; ρ; L⟩     (Not-True)
⟨E[!false]; ρ; L⟩ → ⟨E[true];  ρ; L⟩     (Not-False)
```

`&&` and `||` are **short-circuit** and evaluated left-to-right, matching the
strict evaluation order of the compiler.

### 5.4 Let-binding

```text
x ∉ dom(ρ)
──────────────────────────────────────────  (Let)
⟨E[let x = v in e']; ρ; L⟩ → ⟨E[e']; ρ[x↦v]; L⟩
```

A linear value bound by `let x = v`: if `v` is linear, add `x` to `L` — the
binding **transfers** ownership into the fresh scope (the source, if any, is
removed from `L`). See §7.

### 5.5 Conditionals

```text
⟨E[if true  {e1} else {e2}]; ρ; L⟩ → ⟨E[e1]; ρ; L⟩   (If-True)
⟨E[if false {e1} else {e2}]; ρ; L⟩ → ⟨E[e2]; ρ; L⟩   (If-False)
```

Only the taken branch evaluates; the other branch is discarded (its linear
values, if any, are **not** moved — fail-closed static checks reject any
linear value that could be stranded by a branch shape).

### 5.6 Match

```text
p(x) matches v, binding x ↦ v
──────────────────────────────────────────  (Match)
⟨E[match v { p(x) => e, ... }]; ρ; L⟩ → ⟨E[e]; ρ[x↦v]; L⟩
```

Single-binding patterns transfer linear ownership of the scrutinee atom to
the binder exactly as `let` does. Wildcard positions that could strand a
linear atom are rejected statically.

### 5.7 Blocks, statements, return

```text
⟨E[ e        ; b]; ρ; L⟩ → ⟨E[e; b]; ρ; L⟩  if e is not a value, evaluate it
⟨E[ x  = v   ; b]; ρ; L⟩ → ⟨E[b]; ρ[x↦v]; L⟩          (Assign)
⟨E[ v        ; b]; ρ; L⟩ → ⟨E[b]; ρ; L⟩                (Discard-Value)
⟨E[return v  ; b]; ρ; L⟩ → ⟨v⟩                         (Return)
```

`return` is the only way to discharge the function result. Linear values still
held in `L` at `return` are a **static error** (leak, E0256) unless the value
is the returned one.

### 5.8 Function application (total)

```text
f(x1..xn) = body,  ρ' = ρ[f-params ↦ args],  args linear enter L'
⟨E[f(v1..vn)]; ρ; L⟩ → ⟨body; ρ'; L'⟩                (App)
```

Evaluation is **strict** (arguments reduce to values before the call). The
callee's linear parameters enter a fresh ledger `L'`; the caller's ledger is
suspended and restored on return.

## 6. Evaluation Contexts

```text
E ::= □
    | E ⊕ e | v ⊕ E
    | E ⊙ e | v ⊙ E
    | E && e | v && E | E || e | v || E | !E
    | let x = E in e
    | if E { e1 } else { e2 }
    | match E { p => e, ... }
    | f(v1.., E, .., vn)          (arguments left-to-right)
    | E ; b | x = E ; b | return E ; b
```

The unique decomposition `b = E[redex]` at each step yields a **deterministic**
evaluation order: left-to-right, innermost-first, strict arguments.

## 7. Linear Resource Ledger

Invariant (exactly-once):

```text
For every identifier x ∈ L at any configuration:
  - x is moved at most once along any path (a second use = E0304);
  - x must be consumed (moved, returned, or explicitly released by its
    owning operation) before the owning scope exits (leak = E0256);
  - a dropped linear value is discharged only by its sanctioned release
    (drop for drop-tolerant kinds; mutex_unlock for MutexGuard; token_id /
    token_channel_send / guarded-API call for SystemToken, etc.).
```

Ledger transitions:

```text
let x = v (linear)       : L → L ∪ {x}            (introduce)
x = y (y linear)         : L → (L∖{y}) ∪ {x}      (move)
f(v) where x=arg linear  : L → L∖{x}              (transfer into callee)
return x (x linear)      : L → L∖{x}              (transfer out)
drop(x) (x linear, drop-tolerant kind): L → L∖{x} (release)
```

A configuration that reaches `⟨v⟩` (or `return`) with `L ≠ ∅` violates the
exactly-once invariant; the compiler rejects it statically (E0256) and the
resource analysis enforces it on the CFG.

## 8. Determinism and Agreement

- Each reducible kernel term has exactly one redex decomposition (§6), so the
  small-step relation is **deterministic** up to the unique context split.
- Trap behavior (5.2) matches the interpreter (`integer_overflow` /
  zero-divisor diagnostics) and the native backend (`llvm.*.with.overflow`
  traps / runtime division guard).
- The two verification engines agree with this model on bounded vs unbounded
  checked arithmetic: a contract is Proven only when every arithmetic
  sub-expression's definedness is discharged under the preconditions (see
  Phase E 0.39.80-80b; E0439 divergence is fail-closed).

---

_This appendix is normative for the kernel described in §1. Items outside the
kernel are governed by their own spec documents. Changes must re-pass the
verification gates and record migration impact._
