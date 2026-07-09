use crate::span::Span;

/// Track borrow state with location information for precise diagnostics.
#[derive(Debug, Clone)]
pub(crate) enum BorrowState {
    Unborrowed,
    BorrowedImm { span: Span },
    BorrowedMut { span: Span },
}

impl BorrowState {}
