use crate::diagnostic::codes;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::fmt;

/// Context data shared by all InterpError variants.
#[derive(Debug, Clone)]
pub struct ErrorContext {
    pub msg: String,
    pub function: Option<String>,
    pub operation: Option<String>,
    pub help: Option<String>,
    pub call_stack: Vec<String>,
}

/// A structured interpreter error with a typed error code.
///
/// Each variant maps to an E0xxx error code (defined in diagnostic::codes).
/// Use the factory methods (e.g. `InterpError::div_by_zero()`) to create
/// specific variants, or `InterpError::new(msg)` for generic runtime errors.
#[derive(Debug, Clone)]
pub enum InterpError {
    /// Generic runtime error (E0800).
    Generic(ErrorContext),
    /// Division by zero at runtime (E0801).
    DivisionByZero(ErrorContext),
    /// Integer overflow at runtime (E0802).
    IntegerOverflow(ErrorContext),
    /// Index out of bounds at runtime (E0803).
    IndexOutOfBounds(ErrorContext),
    /// Wrong argument count at runtime (E0804).
    WrongArgCount(ErrorContext),
    /// Non-exhaustive match at runtime (E0805).
    NonExhaustiveMatch(ErrorContext),
    /// Concurrent lock error (E0806).
    LockError(ErrorContext),
    /// Arena escape at runtime (E0807).
    ArenaEscape(ErrorContext),
    /// Contract violation (requires/ensures) at runtime (E0808).
    ContractViolation(ErrorContext),
    /// Field not found at runtime (E0809).
    FieldNotFound(ErrorContext),
    /// Runtime I/O error (E0810).
    IoError(ErrorContext),
    /// Builtin function runtime error (E0811).
    BuiltinError(ErrorContext),
    /// Type mismatch at runtime (E0812).
    TypeMismatch(ErrorContext),
    /// Floating-point error (NaN, infinity) at runtime (E0813).
    FloatError(ErrorContext),
    /// Slice out of bounds at runtime (E0814).
    SliceError(ErrorContext),
}

impl InterpError {
    /// Return the error code for this variant.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Generic(_) => codes::E0800,
            Self::DivisionByZero(_) => codes::E0801,
            Self::IntegerOverflow(_) => codes::E0802,
            Self::IndexOutOfBounds(_) => codes::E0803,
            Self::WrongArgCount(_) => codes::E0804,
            Self::NonExhaustiveMatch(_) => codes::E0805,
            Self::LockError(_) => codes::E0806,
            Self::ArenaEscape(_) => codes::E0807,
            Self::ContractViolation(_) => codes::E0808,
            Self::FieldNotFound(_) => codes::E0809,
            Self::IoError(_) => codes::E0810,
            Self::BuiltinError(_) => codes::E0811,
            Self::TypeMismatch(_) => codes::E0812,
            Self::FloatError(_) => codes::E0813,
            Self::SliceError(_) => codes::E0814,
        }
    }

    /// Borrow the inner `ErrorContext`.
    pub fn ctx(&self) -> &ErrorContext {
        match self {
            Self::Generic(c)
            | Self::DivisionByZero(c)
            | Self::IntegerOverflow(c)
            | Self::IndexOutOfBounds(c)
            | Self::WrongArgCount(c)
            | Self::NonExhaustiveMatch(c)
            | Self::LockError(c)
            | Self::ArenaEscape(c)
            | Self::ContractViolation(c)
            | Self::FieldNotFound(c)
            | Self::IoError(c)
            | Self::BuiltinError(c)
            | Self::TypeMismatch(c)
            | Self::FloatError(c)
            | Self::SliceError(c) => c,
        }
    }

    /// Mutably borrow the inner `ErrorContext`.
    pub fn ctx_mut(&mut self) -> &mut ErrorContext {
        match self {
            Self::Generic(c)
            | Self::DivisionByZero(c)
            | Self::IntegerOverflow(c)
            | Self::IndexOutOfBounds(c)
            | Self::WrongArgCount(c)
            | Self::NonExhaustiveMatch(c)
            | Self::LockError(c)
            | Self::ArenaEscape(c)
            | Self::ContractViolation(c)
            | Self::FieldNotFound(c)
            | Self::IoError(c)
            | Self::BuiltinError(c)
            | Self::TypeMismatch(c)
            | Self::FloatError(c)
            | Self::SliceError(c) => c,
        }
    }

    /// The human-readable error message.
    pub fn message(&self) -> &str {
        &self.ctx().msg
    }

    /// Create a generic runtime error (code E0800).
    pub fn new(msg: impl Into<String>) -> Self {
        Self::Generic(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a generic runtime error with an operation context.
    pub fn with_op(msg: impl Into<String>, operation: impl Into<String>) -> Self {
        Self::Generic(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: Some(operation.into()),
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Set the function context.
    pub fn in_func(mut self, func_name: impl Into<String>) -> Self {
        self.ctx_mut().function = Some(func_name.into());
        self
    }

    /// Set the operation context.
    pub fn at_op(mut self, operation: impl Into<String>) -> Self {
        self.ctx_mut().operation = Some(operation.into());
        self
    }

    /// Set the help message.
    pub fn with_help_msg(mut self, help: impl Into<String>) -> Self {
        self.ctx_mut().help = Some(help.into());
        self
    }

    /// Set the call stack.
    pub fn with_call_stack(mut self, stack: Vec<String>) -> Self {
        self.ctx_mut().call_stack = stack;
        self
    }

    // ── Variant-specific constructors ──

    /// Create a division-by-zero error (E0801).
    pub fn div_by_zero() -> Self {
        Self::DivisionByZero(ErrorContext {
            msg: "division by zero".into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create an integer overflow error (E0802).
    pub fn integer_overflow(msg: impl Into<String>) -> Self {
        Self::IntegerOverflow(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create an index-out-of-bounds error (E0803).
    pub fn index_out_of_bounds(msg: impl Into<String>) -> Self {
        Self::IndexOutOfBounds(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a wrong-argument-count error (E0804).
    pub fn wrong_arg_count(msg: impl Into<String>) -> Self {
        Self::WrongArgCount(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a lock error (E0806).
    pub fn lock_error(msg: impl Into<String>) -> Self {
        Self::LockError(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create an arena escape error (E0807).
    pub fn arena_escape(msg: impl Into<String>) -> Self {
        Self::ArenaEscape(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a contract violation error (E0808).
    pub fn contract_violation(msg: impl Into<String>) -> Self {
        Self::ContractViolation(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a field-not-found error (E0809).
    pub fn field_not_found(msg: impl Into<String>) -> Self {
        Self::FieldNotFound(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a runtime I/O error (E0810).
    pub fn io_error(msg: impl Into<String>) -> Self {
        Self::IoError(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a builtin function runtime error (E0811).
    pub fn builtin_error(msg: impl Into<String>) -> Self {
        Self::BuiltinError(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a runtime type mismatch error (E0812).
    pub fn type_mismatch(msg: impl Into<String>) -> Self {
        Self::TypeMismatch(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a floating-point error (NaN, infinity) (E0813).
    pub fn float_error(msg: impl Into<String>) -> Self {
        Self::FloatError(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }

    /// Create a slice error (E0814).
    pub fn slice_error(msg: impl Into<String>) -> Self {
        Self::SliceError(ErrorContext {
            msg: msg.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        })
    }
}

impl fmt::Display for InterpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ctx = self.ctx();
        write!(f, "[{}] {}", self.code(), ctx.msg)?;
        if let Some(op) = &ctx.operation {
            write!(f, " (in {})", op)?;
        }
        if let Some(func) = &ctx.function {
            write!(f, " [{}]", func)?;
        }
        if let Some(help) = &ctx.help {
            write!(f, "\n  help: {}", help)?;
        }
        if !ctx.call_stack.is_empty() {
            write!(f, "\n  call stack:")?;
            for frame in ctx.call_stack.iter().rev() {
                write!(f, "\n    at {}", frame)?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for InterpError {}

impl From<String> for InterpError {
    fn from(msg: String) -> Self {
        Self::new(msg)
    }
}

impl From<&str> for InterpError {
    fn from(msg: &str) -> Self {
        Self::new(msg)
    }
}

impl InterpError {
    /// Convert this error to a Diagnostic for rich terminal output.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let ctx = self.ctx();
        let mut message = ctx.msg.clone();

        if let Some(op) = &ctx.operation {
            message = format!("{} (in {})", message, op);
        }
        if let Some(func) = &ctx.function {
            message = format!("{} [{}]", message, func);
        }

        let mut diag = Diagnostic::error_code(self.code(), message, Span::UNKNOWN);

        if let Some(help) = &ctx.help {
            diag = diag.with_help(help.clone());
        }
        if !ctx.call_stack.is_empty() {
            let stack_str: Vec<&str> = ctx.call_stack.iter().map(|s| s.as_str()).collect();
            diag = diag.with_note(
                format!("call stack: {}", stack_str.join(" → ")),
                Span::UNKNOWN,
            );
        }

        diag
    }
}

impl From<&InterpError> for Diagnostic {
    fn from(e: &InterpError) -> Self {
        e.to_diagnostic()
    }
}

impl From<InterpError> for Diagnostic {
    fn from(e: InterpError) -> Self {
        e.to_diagnostic()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_error() {
        let err = InterpError::new("something went wrong");
        assert_eq!(err.message(), "something went wrong");
        assert_eq!(err.code(), codes::E0800);
        assert!(err.ctx().function.is_none());
    }

    #[test]
    fn test_error_with_context() {
        let err = InterpError::with_op("index out of bounds", "array access")
            .in_func("main")
            .with_help_msg("use a valid index");
        assert_eq!(err.message(), "index out of bounds");
        assert_eq!(err.ctx().operation.as_deref().unwrap(), "array access");
        assert_eq!(err.ctx().function.as_deref().unwrap(), "main");
        assert_eq!(err.ctx().help.as_deref().unwrap(), "use a valid index");
    }

    #[test]
    fn test_error_display() {
        let err = InterpError::with_op("division by zero", "arithmetic")
            .in_func("compute")
            .with_call_stack(vec!["main".into(), "compute".into()]);
        let display = format!("{}", err);
        assert!(display.contains("[E0800]"));
        assert!(display.contains("division by zero"));
        assert!(display.contains("in arithmetic"));
        assert!(display.contains("[compute]"));
        assert!(display.contains("call stack:"));
    }

    #[test]
    fn test_from_string() {
        let err: InterpError = "simple error".into();
        assert_eq!(err.message(), "simple error");
    }

    #[test]
    fn test_specific_variant_codes() {
        assert_eq!(InterpError::div_by_zero().code(), codes::E0801);
        assert_eq!(
            InterpError::integer_overflow("overflow").code(),
            codes::E0802
        );
        assert_eq!(InterpError::index_out_of_bounds("oob").code(), codes::E0803);
        assert_eq!(InterpError::wrong_arg_count("args").code(), codes::E0804);
        assert_eq!(InterpError::lock_error("lock").code(), codes::E0806);
        assert_eq!(InterpError::arena_escape("escape").code(), codes::E0807);
        assert_eq!(
            InterpError::contract_violation("violation").code(),
            codes::E0808
        );
        assert_eq!(InterpError::io_error("io").code(), codes::E0810);
        assert_eq!(InterpError::type_mismatch("t").code(), codes::E0812);
        assert_eq!(InterpError::float_error("f").code(), codes::E0813);
        assert_eq!(InterpError::slice_error("s").code(), codes::E0814);
    }
}
