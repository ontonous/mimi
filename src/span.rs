use std::fmt;

/// A source code span representing a range of text in the source file.
/// Uses compact representation: start position (line, col) + end position (line, col).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

impl Span {
    /// Create a new span with explicit start and end positions.
    pub fn new(start_line: usize, start_col: usize, end_line: usize, end_col: usize) -> Self {
        Self {
            start_line,
            start_col,
            end_line,
            end_col,
        }
    }

    /// Create a single-point span (start == end).
    pub fn single(line: usize, col: usize) -> Self {
        Self {
            start_line: line,
            start_col: col,
            end_line: line,
            end_col: col,
        }
    }

    /// Create a span from a single point to another point.
    pub fn to(&self, other: &Span) -> Self {
        Self {
            start_line: self.start_line,
            start_col: self.start_col,
            end_line: other.end_line,
            end_col: other.end_col,
        }
    }

    /// Create a span that covers from self's start to other's end.
    pub fn until(&self, other: &Span) -> Self {
        Self {
            start_line: self.start_line,
            start_col: self.start_col,
            end_line: other.end_line,
            end_col: other.end_col,
        }
    }

    /// Check if this span contains a given position.
    pub fn contains(&self, line: usize, col: usize) -> bool {
        if line < self.start_line || line > self.end_line {
            return false;
        }
        if line == self.start_line && col < self.start_col {
            return false;
        }
        if line == self.end_line && col > self.end_col {
            return false;
        }
        true
    }

    /// Get the width of the span in characters.
    /// For multi-line spans, returns the total width across all lines.
    pub fn width(&self) -> usize {
        if self.start_line == self.end_line {
            self.end_col.saturating_sub(self.start_col)
        } else {
            // Multi-line: approximate total width including newline characters.
            // Uses a reasonable default line width of 120 characters (most
            // modern terminals). This is an approximation — the actual source
            // line width is not available here. The impact is only on
            // diagnostics display (wrapping/truncation) and is not
            // semantically significant.
            const LINE_WIDTH: usize = 120;
            let lines = self.end_line.saturating_sub(self.start_line);
            // First line width + intervening lines + last line + newlines
            let first_line = LINE_WIDTH.saturating_sub(self.start_col);
            let mid_lines = lines.saturating_sub(1).saturating_mul(LINE_WIDTH);
            first_line
                .saturating_add(mid_lines)
                .saturating_add(self.end_col)
                .saturating_add(lines)
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.start_line == self.end_line {
            if self.start_col == self.end_col {
                write!(f, "{}:{}", self.start_line, self.start_col)
            } else {
                write!(f, "{}:{}-{}", self.start_line, self.start_col, self.end_col)
            }
        } else {
            write!(
                f,
                "{}:{}-{}:{}",
                self.start_line, self.start_col, self.end_line, self.end_col
            )
        }
    }
}

/// Convert from (line, col) token positions to a Span.
/// Assumes line/col are 1-indexed (as in the lexer).
impl From<(usize, usize)> for Span {
    fn from((line, col): (usize, usize)) -> Self {
        Self::single(line, col)
    }
}
