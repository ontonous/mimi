use std::fmt;
use crate::diagnostic::{Diagnostic, Severity};
use crate::span::Span;

/// A structured interpreter error with context information.
#[derive(Debug, Clone)]
pub struct InterpError {
    /// The error message
    pub message: String,
    /// The function where the error occurred (if known)
    pub function: Option<String>,
    /// The operation that failed (e.g., "index access", "field access", "addition")
    pub operation: Option<String>,
    /// Suggested fix or hint
    pub help: Option<String>,
    /// Call stack at the time of the error
    pub call_stack: Vec<String>,
}

impl InterpError {
    /// Create a new error with just a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
        }
    }

    /// Create an error with a message and operation context.
    pub fn with_op(message: impl Into<String>, operation: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            function: None,
            operation: Some(operation.into()),
            help: None,
            call_stack: Vec::new(),
        }
    }

    /// Set the function context.
    pub fn in_func(mut self, func_name: impl Into<String>) -> Self {
        self.function = Some(func_name.into());
        self
    }

    /// Set the operation context.
    pub fn at_op(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    /// Set the help message.
    pub fn with_help_msg(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Set the call stack.
    pub fn with_call_stack(mut self, stack: Vec<String>) -> Self {
        self.call_stack = stack;
        self
    }
}

impl fmt::Display for InterpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(op) = &self.operation {
            write!(f, " (in {})", op)?;
        }
        if let Some(func) = &self.function {
            write!(f, " [{}]", func)?;
        }
        if let Some(help) = &self.help {
            write!(f, "\n  help: {}", help)?;
        }
        if !self.call_stack.is_empty() {
            write!(f, "\n  call stack:")?;
            for frame in self.call_stack.iter().rev() {
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
        let mut message = self.message.clone();

        // Add operation context to message
        if let Some(op) = &self.operation {
            message = format!("{} (in {})", message, op);
        }

        // Add function context to message
        if let Some(func) = &self.function {
            message = format!("{} [{}]", message, func);
        }

        let mut diag = Diagnostic::error(message, Span::single(0, 0));

        // Add help if available
        if let Some(help) = &self.help {
            diag = diag.with_help(help.clone());
        }

        // Add call stack as notes
        if !self.call_stack.is_empty() {
            let stack_str: Vec<&str> = self.call_stack.iter().map(|s| s.as_str()).collect();
            diag = diag.with_note(
                format!("call stack: {}", stack_str.join(" → ")),
                Span::single(0, 0),
            );
        }

        diag
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_error() {
        let err = InterpError::new("something went wrong");
        assert_eq!(err.message, "something went wrong");
        assert!(err.function.is_none());
    }

    #[test]
    fn test_error_with_context() {
        let err = InterpError::with_op("index out of bounds", "array access")
            .in_func("main")
            .with_help_msg("use a valid index");
        assert_eq!(err.message, "index out of bounds");
        assert_eq!(err.operation.unwrap(), "array access");
        assert_eq!(err.function.unwrap(), "main");
        assert_eq!(err.help.unwrap(), "use a valid index");
    }

    #[test]
    fn test_error_display() {
        let err = InterpError::with_op("division by zero", "arithmetic")
            .in_func("compute")
            .with_call_stack(vec!["main".into(), "compute".into()]);
        let display = format!("{}", err);
        assert!(display.contains("division by zero"));
        assert!(display.contains("in arithmetic"));
        assert!(display.contains("[compute]"));
        assert!(display.contains("call stack:"));
    }

    #[test]
    fn test_from_string() {
        let err: InterpError = "simple error".into();
        assert_eq!(err.message, "simple error");
    }
}
