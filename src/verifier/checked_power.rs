//! Exact symbolic term and definedness for Mimi integer `**`.
//!
//! This module is intentionally independent of both consumers: Resolved and
//! Canonical MIR can use the same arithmetic encoding without sharing AST or
//! MIR traversal. Callers must evaluate/collect the left operand first, then
//! the right operand, and must collect all operand and checked-conversion
//! definedness before adding this operator's `defined` predicate. The `Int`
//! arguments are the already-evaluated mathematical values of the two typed
//! machine-integer operands.
//!
//! Two explicit profiles are available while the language authority settles
//! the `**` width rule. This phase selects `VmNativeI64ThenI32Wrap` as its
//! compatibility profile because the current production L1 fixture locks
//! `2 ** 31` to i32::MIN; that is a phase implementation anchor, not a
//! long-term normative decision. The profile captures the VM/native path:
//! Rust `i64::checked_pow(u32::try_from(exponent)?)`, followed by low-32-bit
//! wrap for i32. `CheckedSourceWidth` captures checked_pow at the source
//! i32/i64 width. Both reject negative and >u32::MAX exponents.
//! For either profile, checked intermediate multiplications are modeled for
//! exponents 0 through signed-width-minus-one. For larger convertible
//! exponents, `|base| >= 2` necessarily overflows in a checked factor-square
//! or accumulator multiply in Rust's exponentiation-by-squaring algorithm;
//! bases -1, 0, and 1 remain safe and have a closed form.
//!
//! No solver query is performed here. Consumers must preserve `unknown` as
//! inconclusive and must not turn it into a successful definedness proof.

use z3::ast::{Bool, Int};

/// Static source width selected by Mimi's checked type information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckedPowerWidth {
    I32,
    I64,
}

impl CheckedPowerWidth {
    fn bits(self) -> u32 {
        match self {
            Self::I32 => 32,
            Self::I64 => 64,
        }
    }

    fn min(self) -> i64 {
        match self {
            Self::I32 => i32::MIN as i64,
            Self::I64 => i64::MIN,
        }
    }

    fn max(self) -> i64 {
        match self {
            Self::I32 => i32::MAX as i64,
            Self::I64 => i64::MAX,
        }
    }
}

/// The operation-width decision is explicit because the normative small-step
/// kernel omits `**`, while current L1 VM/native tests require a checked-i64
/// computation followed by i32 narrowing. Do not select a profile from this
/// module's default: the caller must name the authority-backed rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckedPowerProfile {
    /// Candidate source-width checked rule: the intermediate/result range is
    /// exactly i32 or i64 as selected by typed Mimi IR.
    #[cfg(test)]
    CheckedSourceWidth,
    /// Existing VM/native implementation: checked i64 intermediates and an
    /// i32 wrap after success when the typed result width is i32.
    VmNativeI64ThenI32Wrap,
}

/// The explicit 0.1.11 implementation anchor. Keep the spelling in proof
/// artifacts so `checked_i32` cannot conceal i64 intermediate checks.
pub(crate) const POWER_PROFILE_0_1_11_COMPAT: CheckedPowerProfile =
    CheckedPowerProfile::VmNativeI64ThenI32Wrap;

/// Solver-visible failure predicates for the operation. The three runtime trap
/// classes are disjoint by construction and are all guarded by the typed
/// source-range premise. `operands_outside_source_width` is a separate
/// encoding-premise failure, never a runtime Power trap.
/// Consumers must prove the range premise before interpreting runtime faults
/// or using `value`; a violating model is an invalid symbolic input, not E0800
/// or E0802.
///
/// Under the compatibility profile, negative / non-u32 exponents are generic
/// `E0800` and checked-power overflow is `E0802`. The staged native adapter
/// routes those cases to typed runtime traps; active-source parity remains
/// unverified until the parent integrates and runs the source matrix.
#[derive(Debug, Clone)]
pub(crate) struct CheckedPowerFaults {
    /// The symbolic operands violate the static operation width. Well-typed
    /// callers normally discharge this from their parameter/value ranges;
    /// this is kept separate from the runtime PowInt trap partitions.
    pub operands_outside_source_width: Bool,
    /// `u32::try_from(exponent)` rejects a negative, in-source-width exponent
    /// (VM E0800). False when a typed operand violates its source range.
    pub negative_exponent: Bool,
    /// `u32::try_from(exponent)` rejects an in-source-width exponent above
    /// `u32::MAX` (VM E0800; reachable for i64 source operands only). False
    /// when a typed operand violates its source range.
    pub exponent_above_u32: Bool,
    /// At least one checked multiplication in the selected algorithm width
    /// overflows, assuming the exponent conversion succeeds.
    pub checked_multiply_overflow: Bool,
}

/// Exact result term plus the operation's normal-completion predicate.
#[derive(Debug, Clone)]
pub(crate) struct CheckedPowerEncoding {
    /// Equals the backend result whenever `defined` is true. For i32 this is
    /// the low-32-bit wrap of the checked i64 PowInt result, as locked by the
    /// VM/native L1 tests.
    pub value: Int,
    /// True iff the typed operands are in their source ranges, the exponent
    /// converts to u32, and every checked multiply completes in the selected
    /// profile's algorithm width.
    pub defined: Bool,
    pub faults: CheckedPowerFaults,
}

const U32_MAX_I64: i64 = u32::MAX as i64;

/// Encode integer power using the currently L1-anchored VM/native operator
/// behavior. Operand *evaluation* is deliberately outside this pure function:
/// a Resolved/MIR visitor must retain left-to-right operand definedness and
/// effect receipts, including any checked `NumericNarrowChecked` conversion.
pub(crate) fn encode_checked_integer_power(
    base: &Int,
    exponent: &Int,
    source_width: CheckedPowerWidth,
    profile: CheckedPowerProfile,
) -> CheckedPowerEncoding {
    let source_min = Int::from_i64(source_width.min());
    let source_max = Int::from_i64(source_width.max());
    let operands_in_width = Bool::and(&[
        &base.ge(&source_min),
        &base.le(&source_max),
        &exponent.ge(&source_min),
        &exponent.le(&source_max),
    ]);

    let zero = Int::from_i64(0);
    let one = Int::from_i64(1);
    let two = Int::from_i64(2);
    let checked_width = match profile {
        #[cfg(test)]
        CheckedPowerProfile::CheckedSourceWidth => source_width,
        CheckedPowerProfile::VmNativeI64ThenI32Wrap => CheckedPowerWidth::I64,
    };
    let checked_min = Int::from_i64(checked_width.min());
    let checked_max = Int::from_i64(checked_width.max());
    let enumerated_exponent_max = checked_width.bits() - 1;

    let mut finite_value = Int::from_i64(0);
    let mut finite_safe_cases = Vec::with_capacity(enumerated_exponent_max as usize + 1);

    // PowInt is Rust checked_pow's exponentiation-by-squaring algorithm. The
    // small finite cases carry a range check for *each* accumulator multiply
    // and factor square, not merely for the final mathematical power.
    for n in (0..=enumerated_exponent_max).rev() {
        let (term, multiplications_fit) = checked_pow_fixed(base, n, &checked_min, &checked_max);
        let at_n = exponent.eq(&Int::from_i64(n as i64));
        finite_safe_cases.push(Bool::and(&[&at_n, &multiplications_fit]));
        finite_value = at_n.ite(&term, &finite_value);
    }

    // For n >= the selected signed width, every |base| >= 2 grows past that
    // width. Rust's dynamic checked_pow loop must trap on a factor-square or
    // accumulator multiply before returning. The remaining bases are closed
    // forms and all their intermediate products stay in {-1, 0, 1}.
    let exponent_at_least_limit = exponent.ge(&Int::from_i64(checked_width.bits() as i64));
    let base_is_zero = base.eq(&zero);
    let base_is_one = base.eq(&one);
    let minus_one = Int::from_i64(-1);
    let base_is_minus_one = base.eq(&minus_one);
    let special_base = Bool::or(&[&base_is_zero, &base_is_one, &base_is_minus_one]);
    let exponent_even = exponent.modulo(&two).eq(&zero);
    let minus_one_power = exponent_even.ite(&one, &minus_one);
    let large_value = base_is_zero.ite(&zero, &base_is_minus_one.ite(&minus_one_power, &one));
    let large_safe = Bool::and(&[&exponent_at_least_limit, &special_base]);
    let checked_value = exponent_at_least_limit.ite(&large_value, &finite_value);
    let finite_safe = or_all(&finite_safe_cases);
    let checked_pow_fits = Bool::or(&[&finite_safe, &large_safe]);

    let nonnegative_exponent = exponent.ge(&zero);
    let exponent_fits_u32 = exponent.le(&Int::from_i64(U32_MAX_I64));
    let exponent_converts = Bool::and(&[&nonnegative_exponent, &exponent_fits_u32]);
    let defined = Bool::and(&[&operands_in_width, &exponent_converts, &checked_pow_fits]);

    let checked_multiply_overflow = Bool::and(&[
        &operands_in_width,
        &exponent_converts,
        &checked_pow_fits.not(),
    ]);
    let negative_exponent = Bool::and(&[&operands_in_width, &nonnegative_exponent.not()]);
    let exponent_above_u32 = Bool::and(&[
        &operands_in_width,
        &exponent.gt(&Int::from_i64(U32_MAX_I64)),
    ]);
    let faults = CheckedPowerFaults {
        operands_outside_source_width: operands_in_width.not(),
        negative_exponent,
        exponent_above_u32,
        checked_multiply_overflow,
    };

    let value = match (source_width, profile) {
        (CheckedPowerWidth::I32, CheckedPowerProfile::VmNativeI64ThenI32Wrap) => {
            wrap_signed_i32(&checked_value)
        }
        _ => checked_value,
    };

    CheckedPowerEncoding {
        value,
        defined,
        faults,
    }
}

/// Mirrors `i64::checked_pow` for one concrete u32 exponent, returning the
/// exact mathematical result term and the conjunction of its checked
/// intermediate-operation range checks.
fn checked_pow_fixed(
    base: &Int,
    exponent: u32,
    checked_min: &Int,
    checked_max: &Int,
) -> (Int, Bool) {
    let one = Int::from_i64(1);
    let mut acc = one;
    let mut factor = base.clone();
    let mut remaining = exponent;
    let mut checks = Vec::new();

    while remaining != 0 {
        if remaining & 1 == 1 {
            acc = Int::mul(&[&acc, &factor]);
            checks.push(fits_width(&acc, checked_min, checked_max));
            if remaining == 1 {
                break;
            }
        }

        remaining /= 2;
        factor = Int::mul(&[&factor, &factor]);
        checks.push(fits_width(&factor, checked_min, checked_max));
    }

    (acc, and_all(&checks))
}

fn fits_width(value: &Int, min: &Int, max: &Int) -> Bool {
    Bool::and(&[&value.ge(min), &value.le(max)])
}

/// Reproduce an LLVM truncation / VM `WrapI32` as signed mathematical i32.
fn wrap_signed_i32(value: &Int) -> Int {
    const MODULUS: i64 = 1_i64 << 32;
    const HALF: i64 = 1_i64 << 31;
    let shifted = Int::add(&[value, &Int::from_i64(HALF)]);
    Int::sub(&[
        &shifted.modulo(&Int::from_i64(MODULUS)),
        &Int::from_i64(HALF),
    ])
}

fn and_all(conditions: &[Bool]) -> Bool {
    if conditions.is_empty() {
        Bool::from_bool(true)
    } else {
        let refs = conditions.iter().collect::<Vec<_>>();
        Bool::and(&refs)
    }
}

fn or_all(conditions: &[Bool]) -> Bool {
    if conditions.is_empty() {
        Bool::from_bool(false)
    } else {
        let refs = conditions.iter().collect::<Vec<_>>();
        Bool::or(&refs)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        encode_checked_integer_power, CheckedPowerProfile, CheckedPowerWidth,
        POWER_PROFILE_0_1_11_COMPAT,
    };
    use z3::ast::{Ast, Int};
    use z3::{SatResult, Solver};

    fn constant_encoding(
        base: i64,
        exponent: i64,
        width: CheckedPowerWidth,
        profile: CheckedPowerProfile,
    ) -> (bool, Option<i64>) {
        let encoding = encode_checked_integer_power(
            &Int::from_i64(base),
            &Int::from_i64(exponent),
            width,
            profile,
        );
        (
            encoding
                .defined
                .simplify()
                .as_bool()
                .expect("constant definedness"),
            encoding.value.simplify().as_i64(),
        )
    }

    fn compat_i32(base: i64, exponent: i64) -> (bool, Option<i64>) {
        constant_encoding(
            base,
            exponent,
            CheckedPowerWidth::I32,
            POWER_PROFILE_0_1_11_COMPAT,
        )
    }

    fn compat_i64(base: i64, exponent: i64) -> (bool, Option<i64>) {
        constant_encoding(
            base,
            exponent,
            CheckedPowerWidth::I64,
            POWER_PROFILE_0_1_11_COMPAT,
        )
    }

    #[test]
    fn i32_is_checked_in_i64_then_wrapped_at_the_source_width() {
        assert_eq!(compat_i32(2, 31), (true, Some(i32::MIN as i64)));
        assert_eq!(compat_i32(-2, 63), (true, Some(0)));
        // The term is not observable when `defined` is false. Only the trap
        // predicate is part of the operation contract for this input.
        assert!(!compat_i32(2, 63).0);
    }

    #[test]
    fn i64_checked_power_keeps_the_minimum_negative_result() {
        assert_eq!(compat_i64(-2, 63), (true, Some(i64::MIN)));
        assert!(!compat_i64(2, 63).0);
        assert!(!compat_i64(2, 64).0);
    }

    #[test]
    fn negative_and_non_u32_exponents_are_not_defined_even_for_special_bases() {
        assert!(!compat_i64(1, -1).0);
        assert!(!compat_i64(1, u32::MAX as i64 + 1).0);
        assert_eq!(compat_i64(1, u32::MAX as i64), (true, Some(1)));
    }

    #[test]
    fn large_exponents_use_only_the_proven_safe_special_bases() {
        assert_eq!(compat_i64(0, u32::MAX as i64), (true, Some(0)));
        assert_eq!(compat_i64(-1, u32::MAX as i64), (true, Some(-1)));
        assert_eq!(compat_i64(-1, u32::MAX as i64 - 1), (true, Some(1)));
        assert!(!compat_i32(2, 64).0);
    }

    #[test]
    fn source_width_checked_profile_is_distinct_and_exact() {
        assert!(
            !constant_encoding(
                2,
                31,
                CheckedPowerWidth::I32,
                CheckedPowerProfile::CheckedSourceWidth,
            )
            .0
        );
        assert_eq!(
            constant_encoding(
                -2,
                31,
                CheckedPowerWidth::I32,
                CheckedPowerProfile::CheckedSourceWidth,
            ),
            (true, Some(i32::MIN as i64))
        );
        assert_eq!(
            constant_encoding(
                -2,
                63,
                CheckedPowerWidth::I64,
                CheckedPowerProfile::CheckedSourceWidth,
            ),
            (true, Some(i64::MIN))
        );
    }

    #[test]
    fn fault_predicates_distinguish_negative_cap_and_checked_overflow() {
        let negative = encode_checked_integer_power(
            &Int::from_i64(1),
            &Int::from_i64(-1),
            CheckedPowerWidth::I64,
            CheckedPowerProfile::VmNativeI64ThenI32Wrap,
        );
        assert!(negative
            .faults
            .negative_exponent
            .simplify()
            .as_bool()
            .unwrap());
        assert!(!negative
            .faults
            .exponent_above_u32
            .simplify()
            .as_bool()
            .unwrap());
        assert!(!negative
            .faults
            .checked_multiply_overflow
            .simplify()
            .as_bool()
            .unwrap());

        let over_cap = encode_checked_integer_power(
            &Int::from_i64(1),
            &Int::from_i64(u32::MAX as i64 + 1),
            CheckedPowerWidth::I64,
            CheckedPowerProfile::VmNativeI64ThenI32Wrap,
        );
        assert!(!over_cap
            .faults
            .negative_exponent
            .simplify()
            .as_bool()
            .unwrap());
        assert!(over_cap
            .faults
            .exponent_above_u32
            .simplify()
            .as_bool()
            .unwrap());
        assert!(!over_cap
            .faults
            .checked_multiply_overflow
            .simplify()
            .as_bool()
            .unwrap());

        let overflow = encode_checked_integer_power(
            &Int::from_i64(2),
            &Int::from_i64(63),
            CheckedPowerWidth::I64,
            CheckedPowerProfile::VmNativeI64ThenI32Wrap,
        );
        assert!(!overflow
            .faults
            .negative_exponent
            .simplify()
            .as_bool()
            .unwrap());
        assert!(!overflow
            .faults
            .exponent_above_u32
            .simplify()
            .as_bool()
            .unwrap());
        assert!(overflow
            .faults
            .checked_multiply_overflow
            .simplify()
            .as_bool()
            .unwrap());
    }

    #[test]
    fn out_of_source_width_models_are_not_mislabeled_as_runtime_traps() {
        let encoding = encode_checked_integer_power(
            &Int::from_i64(i32::MAX as i64 + 1),
            &Int::from_i64(-1),
            CheckedPowerWidth::I32,
            CheckedPowerProfile::VmNativeI64ThenI32Wrap,
        );

        assert!(!encoding.defined.simplify().as_bool().unwrap());
        assert!(encoding
            .faults
            .operands_outside_source_width
            .simplify()
            .as_bool()
            .unwrap());
        assert!(!encoding
            .faults
            .negative_exponent
            .simplify()
            .as_bool()
            .unwrap());
        assert!(!encoding
            .faults
            .exponent_above_u32
            .simplify()
            .as_bool()
            .unwrap());
        assert!(!encoding
            .faults
            .checked_multiply_overflow
            .simplify()
            .as_bool()
            .unwrap());
    }

    #[test]
    fn symbolic_square_value_and_definedness_match_every_checked_product() {
        let base = Int::new_const("checked_power_symbolic_base");
        let exponent = Int::from_i64(2);
        let encoding = encode_checked_integer_power(
            &base,
            &exponent,
            CheckedPowerWidth::I64,
            CheckedPowerProfile::VmNativeI64ThenI32Wrap,
        );
        let source_min = Int::from_i64(i64::MIN);
        let source_max = Int::from_i64(i64::MAX);
        let square = Int::mul(&[&base, &base]);
        let expected_defined = z3::ast::Bool::and(&[
            &base.ge(&source_min),
            &base.le(&source_max),
            &square.ge(&source_min),
            &square.le(&source_max),
        ]);

        let solver = Solver::new();
        solver.assert(&encoding.defined.iff(&expected_defined).not());
        assert_eq!(
            solver.check(),
            SatResult::Unsat,
            "the exponent-two definedness predicate must include checked intermediate bounds"
        );

        let solver = Solver::new();
        solver.assert(&encoding.defined);
        solver.assert(&encoding.value.ne(&square));
        assert_eq!(
            solver.check(),
            SatResult::Unsat,
            "defined exponent-two results must equal the exact square"
        );

        let runtime_trap = z3::ast::Bool::or(&[
            &encoding.faults.negative_exponent,
            &encoding.faults.exponent_above_u32,
            &encoding.faults.checked_multiply_overflow,
        ]);
        let invalid_input_or_trap = z3::ast::Bool::or(&[
            &encoding.faults.operands_outside_source_width,
            &runtime_trap,
        ]);
        let solver = Solver::new();
        solver.assert(&encoding.defined.iff(&invalid_input_or_trap.not()).not());
        assert_eq!(
            solver.check(),
            SatResult::Unsat,
            "source-range invalidity and disjoint runtime traps must cover every undefined case"
        );
    }
}
