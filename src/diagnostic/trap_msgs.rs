// Arithmetic-trap message templates (U2, 0.35.44 — SD-7/8/9).
//
// Single source for the human-readable trap *wording*. This file is
// `include!`d by BOTH `diagnostic/codes.rs` (bytecode-VM side, as
// `codes::trap`) and `runtime/mod.rs` (native codegen side) — the latter is
// compiled standalone with `rustc` and therefore cannot reference
// `crate::diagnostic` directly. Keeping the wording in one file means a
// message change lands on both backends at once.
//
// The E-codes themselves (`E0801`/`E0802`/`E0813`) live in `codes.rs`; the
// runtime emits them as byte prefixes matching those constants.

/// SD-8 integer division-by-zero (E0801) — native codegen wording.
pub const INT_DIV_BY_ZERO: &str = "integer division by zero";
/// SD-7 integer overflow (E0802) — native codegen prefix (op name appended).
pub const INT_OVERFLOW_PREFIX: &str = "integer overflow in ";
/// SD-8 MIN/-1 division overflow (E0802) — native codegen wording.
pub const INT_DIV_OVERFLOW: &str = "integer division overflow (MIN / -1)";
/// SD-9 float finiteness (E0813) — native codegen prefix (op name appended).
pub const FLOAT_NOT_FINITE_PREFIX: &str = "float operation produced NaN/Inf in ";
