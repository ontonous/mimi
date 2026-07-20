use super::{compare_op, is_truthy, type_name, values_equal, InterpError, Value};
use crate::core::ir::{ResolvedBinaryOp, ResolvedUnaryOp};

pub(crate) fn apply_binary(
    op: ResolvedBinaryOp,
    left: Value,
    right: Value,
) -> Result<Value, InterpError> {
    let float_op = |left: f64, right: f64, symbol: &str| {
        let value = match op {
            ResolvedBinaryOp::Add => left + right,
            ResolvedBinaryOp::Subtract => left - right,
            ResolvedBinaryOp::Multiply => left * right,
            ResolvedBinaryOp::Divide => {
                if right == 0.0 {
                    return Err(InterpError::div_by_zero());
                }
                left / right
            }
            ResolvedBinaryOp::Power => left.powf(right),
            _ => {
                return Err(InterpError::new(format!(
                    "unsupported floating-point operator '{symbol}'"
                )))
            }
        };
        if value.is_nan() || value.is_infinite() {
            return Err(InterpError::float_error(format!(
                "invalid floating-point result from {left} {symbol} {right}"
            )));
        }
        Ok(Value::Float(value))
    };

    match op {
        ResolvedBinaryOp::LogicalAnd => Ok(Value::Bool(is_truthy(&left) && is_truthy(&right))),
        ResolvedBinaryOp::LogicalOr => Ok(Value::Bool(is_truthy(&left) || is_truthy(&right))),
        ResolvedBinaryOp::Add => match (&left, &right) {
            (Value::String(left), Value::String(right)) => {
                Ok(Value::String(format!("{left}{right}")))
            }
            (Value::Int(left), Value::Int(right)) => left
                .checked_add(*right)
                .map(Value::Int)
                .ok_or_else(|| InterpError::integer_overflow("integer addition overflow")),
            (Value::Float(left), Value::Float(right)) => float_op(*left, *right, "+"),
            (Value::Int(left), Value::Float(right)) => float_op(*left as f64, *right, "+"),
            (Value::Float(left), Value::Int(right)) => float_op(*left, *right as f64, "+"),
            _ => type_error("+", &left, &right),
        },
        ResolvedBinaryOp::Subtract => match (&left, &right) {
            (Value::Int(left), Value::Int(right)) => left
                .checked_sub(*right)
                .map(Value::Int)
                .ok_or_else(|| InterpError::integer_overflow("integer subtraction overflow")),
            (Value::Float(left), Value::Float(right)) => float_op(*left, *right, "-"),
            (Value::Int(left), Value::Float(right)) => float_op(*left as f64, *right, "-"),
            (Value::Float(left), Value::Int(right)) => float_op(*left, *right as f64, "-"),
            _ => type_error("-", &left, &right),
        },
        ResolvedBinaryOp::Multiply => match (&left, &right) {
            (Value::Int(left), Value::Int(right)) => left
                .checked_mul(*right)
                .map(Value::Int)
                .ok_or_else(|| InterpError::integer_overflow("integer multiplication overflow")),
            (Value::Float(left), Value::Float(right)) => float_op(*left, *right, "*"),
            (Value::Int(left), Value::Float(right)) => float_op(*left as f64, *right, "*"),
            (Value::Float(left), Value::Int(right)) => float_op(*left, *right as f64, "*"),
            _ => type_error("*", &left, &right),
        },
        ResolvedBinaryOp::Divide => match (&left, &right) {
            (Value::Int(_), Value::Int(0)) => Err(InterpError::div_by_zero()),
            (Value::Int(left), Value::Int(right)) => left
                .checked_div(*right)
                .map(Value::Int)
                .ok_or_else(|| InterpError::integer_overflow("integer division overflow")),
            (Value::Float(left), Value::Float(right)) => float_op(*left, *right, "/"),
            (Value::Int(left), Value::Float(right)) => float_op(*left as f64, *right, "/"),
            (Value::Float(left), Value::Int(right)) => float_op(*left, *right as f64, "/"),
            _ => type_error("/", &left, &right),
        },
        ResolvedBinaryOp::Remainder => match (&left, &right) {
            (Value::Int(_), Value::Int(0)) => Err(InterpError::div_by_zero()),
            (Value::Int(left), Value::Int(right)) => left
                .checked_rem(*right)
                .map(Value::Int)
                .ok_or_else(|| InterpError::integer_overflow("integer remainder overflow")),
            _ => type_error("%", &left, &right),
        },
        ResolvedBinaryOp::Power => match (&left, &right) {
            (Value::Int(_), Value::Int(right)) if *right < 0 => Err(InterpError::new(
                "negative exponent is not supported for integers",
            )),
            (Value::Int(left), Value::Int(right)) => left
                .checked_pow(*right as u32)
                .map(Value::Int)
                .ok_or_else(|| InterpError::integer_overflow("integer power overflow")),
            (Value::Float(left), Value::Float(right)) => float_op(*left, *right, "^"),
            (Value::Int(left), Value::Float(right)) => float_op(*left as f64, *right, "^"),
            (Value::Float(left), Value::Int(right)) => float_op(*left, *right as f64, "^"),
            _ => type_error("^", &left, &right),
        },
        ResolvedBinaryOp::Equal => Ok(Value::Bool(values_equal(&left, &right))),
        ResolvedBinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&left, &right))),
        ResolvedBinaryOp::Less => compare_op(left, right, |order| order.is_lt()),
        ResolvedBinaryOp::Greater => compare_op(left, right, |order| order.is_gt()),
        ResolvedBinaryOp::LessEqual => compare_op(left, right, |order| !order.is_gt()),
        ResolvedBinaryOp::GreaterEqual => compare_op(left, right, |order| !order.is_lt()),
        ResolvedBinaryOp::BitAnd => integer_bit_op("&", left, right, |a, b| a & b),
        ResolvedBinaryOp::BitOr => integer_bit_op("|", left, right, |a, b| a | b),
        ResolvedBinaryOp::BitXor => integer_bit_op("^", left, right, |a, b| a ^ b),
        ResolvedBinaryOp::ShiftLeft => integer_shift("<<", left, right, i64::checked_shl),
        ResolvedBinaryOp::ShiftRight => integer_shift(">>", left, right, i64::checked_shr),
    }
}

pub(crate) fn apply_unary(op: ResolvedUnaryOp, value: Value) -> Result<Value, InterpError> {
    match (op, value) {
        (ResolvedUnaryOp::Negate, Value::Int(value)) => value
            .checked_neg()
            .map(Value::Int)
            .ok_or_else(|| InterpError::integer_overflow("integer negation overflow")),
        (ResolvedUnaryOp::Negate, Value::Float(value)) => Ok(Value::Float(-value)),
        (ResolvedUnaryOp::Not, value) => Ok(Value::Bool(!is_truthy(&value))),
        (ResolvedUnaryOp::BorrowShared | ResolvedUnaryOp::BorrowMutable, _) => Err(
            InterpError::new("typed borrow execution requires a resolved place"),
        ),
        (ResolvedUnaryOp::Dereference, value) => Err(InterpError::new(format!(
            "typed dereference is not available for {}",
            type_name(&value)
        ))),
        (ResolvedUnaryOp::Negate, value) => Err(InterpError::new(format!(
            "cannot negate {}",
            type_name(&value)
        ))),
    }
}

fn type_error(operator: &str, left: &Value, right: &Value) -> Result<Value, InterpError> {
    Err(InterpError::new(format!(
        "cannot apply '{operator}' to {} and {}",
        type_name(left),
        type_name(right)
    )))
}

fn integer_bit_op(
    operator: &str,
    left: Value,
    right: Value,
    apply: impl FnOnce(i64, i64) -> i64,
) -> Result<Value, InterpError> {
    match (left, right) {
        (Value::Int(left), Value::Int(right)) => Ok(Value::Int(apply(left, right))),
        (left, right) => type_error(operator, &left, &right),
    }
}

fn integer_shift(
    operator: &str,
    left: Value,
    right: Value,
    apply: impl FnOnce(i64, u32) -> Option<i64>,
) -> Result<Value, InterpError> {
    match (left, right) {
        (Value::Int(left), Value::Int(right)) => u32::try_from(right)
            .ok()
            .and_then(|right| apply(left, right))
            .map(Value::Int)
            .ok_or_else(|| InterpError::integer_overflow(format!("shift overflow in {operator}"))),
        (left, right) => type_error(operator, &left, &right),
    }
}
