use thiserror::Error;

use crate::diagnostic::Diagnostic;
use crate::span::Span;

pub type MimiResult<T> = std::result::Result<T, CompileError>;

#[derive(Debug, Error)]
pub enum CompileError {
    // === Variable/function resolution ===
    #[error("undefined variable '{0}'")]
    UndefinedVar(String),
    #[error("undefined function '{0}' in codegen")]
    UndefinedFunc(String),
    #[error("type '{0}' not found in codegen")]
    TypeNotFound(String),

    // === Type errors ===
    #[error("actor '{0}' type is not a struct")]
    ActorNotStruct(String),
    #[error("cannot access field on type '{0}'")]
    FieldAccessType(String),
    #[error("cannot dispatch method '{method}' on {obj_type}")]
    MethodDispatch { method: String, obj_type: String },
    #[error("field '{field}' not found on type '{obj_type}'")]
    FieldNotFound { field: String, obj_type: String },
    #[error("{0}")]
    TypeMismatch(String),
    #[error("type '{0}' is not a struct")]
    NotStruct(String),

    // === Argument errors ===
    #[error("{0}")]
    WrongArgCount(String),
    #[error("turbofish for '{name}' expects {expected} type args, got {found}")]
    TurbofishArgCount { name: String, expected: usize, found: usize },

    // === Capabilities ===
    #[error("capability '{0}' has already been consumed")]
    CapConsumed(String),
    #[error("linear capability '{0}' must be consumed (via drop) before end of scope")]
    CapNotConsumed(String),

    // === Platform ===
    #[error("'{0}' requires libc (not available in no_std mode)")]
    RequiresLibc(String),

    // === Expression/operator errors ===
    #[error("unsupported binary operator {0:?}")]
    UnsupportedBinOp(String),
    #[error("unsupported expression in codegen: {0:?}")]
    UnsupportedExpr(String),
    #[error("unsupported statement in codegen: {0}")]
    UnsupportedStmt(String),
    #[error("cannot call {0}: expected a function or closure")]
    NotCallable(String),

    // === Contracts ===
    #[error("contract condition must be boolean, got {0:?}")]
    ContractCondition(String),

    // === Loop control ===
    #[error("break outside of loop")]
    BreakOutsideLoop,
    #[error("continue outside of loop")]
    ContinueOutsideLoop,

    // === Codegen internal errors (E07xx) ===
    #[error("LLVM IR generation error: {0}")]
    LlvmError(String),
    #[error("builtin function error: {0}")]
    BuiltinError(String),
    #[error("extern function '{0}' not declared")]
    ExternNotDeclared(String),

    // === Runtime errors ===
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
    #[error("index out of bounds: index {index} is not valid for {kind} of length {len}")]
    OutOfBounds { index: i64, len: usize, kind: String },
    #[error("division by zero")]
    DivByZero,
    #[error("modulo by zero")]
    ModByZero,

    // === FFI ===
    #[error("FFI wrapper: {0}")]
    FfiWrapper(String),

    // === I/O ===
    #[error("I/O error: {0}")]
    Io(String),

    // === Generic catch-all ===
    #[error("{0}")]
    Generic(String),

    // === Source location wrapper ===
    /// Wraps another error with a source span when the AST node carries one.
    #[error("{error}")]
    WithSpan { error: Box<CompileError>, span: Span },
}

impl CompileError {
    /// Return the Mimi error code associated with this variant.
    pub fn code(&self) -> &'static str {
        use crate::diagnostic::codes::*;
        match self {
            Self::UndefinedVar(_) => E0400,
            Self::UndefinedFunc(_) => E0401,
            Self::TypeNotFound(_) => E0706,
            Self::ActorNotStruct(_) => E0707,
            Self::FieldAccessType(_) => E0707,
            Self::MethodDispatch { .. } => E0708,
            Self::FieldNotFound { .. } => E0220,
            Self::TypeMismatch(_) => E0200,
            Self::NotStruct(_) => E0707,
            Self::WrongArgCount(_) => E0210,
            Self::TurbofishArgCount { .. } => E0239,
            Self::CapConsumed(_) => E0304,
            Self::CapNotConsumed(_) => E0303,
            Self::RequiresLibc(_) => E0750,
            Self::UnsupportedBinOp(_) => E0721,
            Self::UnsupportedExpr(_) => E0722,
            Self::UnsupportedStmt(_) => E0702,
            Self::NotCallable(_) => E0742,
            Self::ContractCondition(_) => E0500,
            Self::BreakOutsideLoop => E0404,
            Self::ContinueOutsideLoop => E0405,
            Self::LlvmError(_) => E0713,
            Self::BuiltinError(_) => E0709,
            Self::ExternNotDeclared(_) => E0710,
            Self::FfiWrapper(_) => E0741,
            Self::AssertionFailed(_) => E0751,
            Self::OutOfBounds { .. } => E0243,
            Self::DivByZero => E0237,
            Self::ModByZero => E0238,
            Self::Io(_) => E0750,
            Self::Generic(_) => E0700,
            Self::WithSpan { error, .. } => error.code(),
        }
    }

    /// Attach a source span to this error.
    pub fn at(self, span: Span) -> Self {
        Self::WithSpan { error: Box::new(self), span }
    }

    /// Convert to a Diagnostic with error code and message.
    /// Errors that carry a span will use it; otherwise a sentinel span is used.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let code = self.code();
        match self {
            Self::WithSpan { error, span } => error.to_diagnostic().with_span(*span),
            _ => Diagnostic::error_code(code, self.to_string(), Span::single(0, 0)),
        }
    }
}

impl From<String> for CompileError {
    fn from(msg: String) -> Self {
        CompileError::Generic(msg)
    }
}

impl From<&str> for CompileError {
    fn from(msg: &str) -> Self {
        CompileError::Generic(msg.to_string())
    }
}

impl From<CompileError> for Diagnostic {
    fn from(err: CompileError) -> Self {
        err.to_diagnostic()
    }
}
