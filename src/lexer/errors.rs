use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexerError {
    TabsNotAllowed {
        line: usize,
        col: usize,
    },
    IndentNotMultipleOfFour {
        line: usize,
        col: usize,
    },
    DedentMismatch {
        line: usize,
        col: usize,
    },
    UnexpectedDollar {
        line: usize,
        col: usize,
    },
    UnexpectedCharacter {
        c: char,
        line: usize,
        col: usize,
    },
    UnterminatedString {
        line: usize,
        col: usize,
    },
    UnterminatedEscape {
        line: usize,
        col: usize,
    },
    UnterminatedFString {
        line: usize,
        col: usize,
    },
    UnterminatedFStringEscape {
        line: usize,
        col: usize,
    },
    UnterminatedInterpolation {
        line: usize,
        col: usize,
    },
    UnterminatedBlockComment {
        line: usize,
        col: usize,
    },
    InvalidEscape {
        escape: String,
        line: usize,
        col: usize,
    },
}

impl fmt::Display for LexerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexerError::TabsNotAllowed { line, col } => write!(
                f,
                "tabs are not allowed for indentation at {}:{}",
                line, col
            ),
            LexerError::IndentNotMultipleOfFour { line, col } => write!(
                f,
                "indentation must be a multiple of 4 spaces at {}:{}",
                line, col
            ),
            LexerError::DedentMismatch { line, col } => write!(
                f,
                "dedent does not match any indentation level at {}:{}",
                line, col
            ),
            LexerError::UnexpectedDollar { line, col } => {
                write!(f, "unexpected '$' at {}:{}", line, col)
            }
            LexerError::UnexpectedCharacter { c, line, col } => {
                write!(f, "unexpected character '{}' at {}:{}", c, line, col)
            }
            LexerError::UnterminatedString { line, col } => {
                write!(f, "unterminated string at {}:{}", line, col)
            }
            LexerError::UnterminatedEscape { line, col } => {
                write!(f, "unterminated escape at {}:{}", line, col)
            }
            LexerError::UnterminatedFString { line, col } => {
                write!(f, "unterminated f-string at {}:{}", line, col)
            }
            LexerError::UnterminatedFStringEscape { line, col } => {
                write!(f, "unterminated escape in f-string at {}:{}", line, col)
            }
            LexerError::UnterminatedInterpolation { line, col } => {
                write!(
                    f,
                    "unterminated interpolation in f-string at {}:{}",
                    line, col
                )
            }
            LexerError::UnterminatedBlockComment { line, col } => {
                write!(f, "unterminated block comment at {}:{}", line, col)
            }
            LexerError::InvalidEscape { escape, line, col } => {
                write!(f, "invalid {} escape at {}:{}", escape, line, col)
            }
        }
    }
}

impl From<LexerError> for String {
    fn from(e: LexerError) -> Self {
        e.to_string()
    }
}

impl std::error::Error for LexerError {}

pub fn tabs_not_allowed(line: usize, col: usize) -> LexerError {
    LexerError::TabsNotAllowed { line, col }
}

pub fn indent_not_multiple_of_four(line: usize, col: usize) -> LexerError {
    LexerError::IndentNotMultipleOfFour { line, col }
}

pub fn dedent_mismatch(line: usize, col: usize) -> LexerError {
    LexerError::DedentMismatch { line, col }
}

pub fn unexpected_dollar(line: usize, col: usize) -> LexerError {
    LexerError::UnexpectedDollar { line, col }
}

pub fn unexpected_character(c: char, line: usize, col: usize) -> LexerError {
    LexerError::UnexpectedCharacter { c, line, col }
}

pub fn unterminated_string(line: usize, col: usize) -> LexerError {
    LexerError::UnterminatedString { line, col }
}

pub fn unterminated_escape(line: usize, col: usize) -> LexerError {
    LexerError::UnterminatedEscape { line, col }
}

pub fn unterminated_fstring(line: usize, col: usize) -> LexerError {
    LexerError::UnterminatedFString { line, col }
}

pub fn unterminated_fstring_escape(line: usize, col: usize) -> LexerError {
    LexerError::UnterminatedFStringEscape { line, col }
}

pub fn unterminated_interpolation(line: usize, col: usize) -> LexerError {
    LexerError::UnterminatedInterpolation { line, col }
}

pub fn unterminated_block_comment(line: usize, col: usize) -> LexerError {
    LexerError::UnterminatedBlockComment { line, col }
}

pub fn invalid_escape(escape: &str, line: usize, col: usize) -> LexerError {
    LexerError::InvalidEscape {
        escape: escape.to_string(),
        line,
        col,
    }
}
