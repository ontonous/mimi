//! Human-readable expression rendering for diagnostics and embedded messages.
//!
//! Contract-violation messages are embedded into compiled binaries at codegen
//! time; they must be readable by humans and log-triage tools alike. Dumping
//! the internal AST via `{:?}` leaks implementation details (`Located`,
//! `AstNodeMeta`, `SourceId`) that are neither stable nor actionable. This
//! module renders expressions back into source-like text instead.
//!
//! Coverage targets the shapes that appear in `requires`/`ensures` clauses
//! (comparisons, arithmetic, calls, field access, `old(...)`, indexing).
//! Exotic nodes degrade to a short placeholder — never a Debug dump.

use crate::ast::{BinOp, Expr, FStringPart, Lit, UnOp};
use crate::core::helpers::fmt_type;

/// Maximum rendered length. Messages are embedded into the binary as global
/// strings; a pathological contract expression must not bloat it.
const MAX_RENDER_LEN: usize = 240;

/// Maximum nesting depth for recursive rendering.
const MAX_DEPTH: usize = 32;

/// Render an expression as readable source-like text.
pub(crate) fn render_expr(expr: &Expr) -> String {
    let mut out = String::new();
    render_into(expr.unlocated(), &mut out, 0);
    if out.len() > MAX_RENDER_LEN {
        // Truncate on a char boundary.
        let cut = (0..=MAX_RENDER_LEN)
            .rev()
            .find(|&i| out.is_char_boundary(i))
            .unwrap_or(0);
        out.truncate(cut);
        out.push_str(" ...");
    }
    out
}

fn render_into(expr: &Expr, out: &mut String, depth: usize) {
    if depth > MAX_DEPTH {
        out.push_str("<expr>");
        return;
    }
    match expr.unlocated() {
        Expr::Literal(lit) => render_lit(lit, out),
        Expr::Ident(name) => out.push_str(name),
        Expr::Binary(op, l, r) => {
            render_into(l, out, depth + 1);
            out.push(' ');
            out.push_str(binop_str(*op));
            out.push(' ');
            render_into(r, out, depth + 1);
        }
        Expr::Unary(op, inner) => {
            out.push_str(unop_str(*op));
            render_into(inner, out, depth + 1);
        }
        Expr::Call(callee, args) => {
            render_into(callee, out, depth + 1);
            out.push('(');
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render_into(arg, out, depth + 1);
            }
            out.push(')');
        }
        Expr::Field(obj, field) => {
            render_into(obj, out, depth + 1);
            out.push('.');
            out.push_str(field);
        }
        Expr::Index(obj, idx) => {
            render_into(obj, out, depth + 1);
            out.push('[');
            render_into(idx, out, depth + 1);
            out.push(']');
        }
        Expr::TupleIndex(obj, idx) => {
            render_into(obj, out, depth + 1);
            out.push('.');
            out.push_str(&idx.to_string());
        }
        Expr::Old(inner) => {
            out.push_str("old(");
            render_into(inner, out, depth + 1);
            out.push(')');
        }
        Expr::Try(inner) => {
            render_into(inner, out, depth + 1);
            out.push('?');
        }
        Expr::Tuple(elems) => {
            out.push('(');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render_into(e, out, depth + 1);
            }
            out.push(')');
        }
        Expr::List(elems) => {
            out.push('[');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render_into(e, out, depth + 1);
            }
            out.push(']');
        }
        Expr::SliceExpr { target, start, end } => {
            render_into(target, out, depth + 1);
            out.push('[');
            if let Some(s) = start {
                render_into(s, out, depth + 1);
            }
            out.push_str("..");
            if let Some(e) = end {
                render_into(e, out, depth + 1);
            }
            out.push(']');
        }
        Expr::Cast(inner, ty) => {
            render_into(inner, out, depth + 1);
            out.push_str(" as ");
            out.push_str(&fmt_type(ty));
        }
        Expr::NamedArg(name, val) => {
            out.push_str(name);
            out.push_str(" = ");
            render_into(val, out, depth + 1);
        }
        _ => out.push_str("<expr>"),
    }
}

fn render_lit(lit: &Lit, out: &mut String) {
    match lit {
        Lit::Int(n) => out.push_str(&n.to_string()),
        Lit::Float(f) => out.push_str(&f.to_string()),
        Lit::Bool(b) => out.push_str(&b.to_string()),
        Lit::String(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    other => out.push(other),
                }
            }
            out.push('"');
        }
        Lit::FString(parts) => {
            out.push_str("f\"");
            for part in parts {
                match part {
                    FStringPart::Text(t) => out.push_str(t),
                    FStringPart::Interp(e) => {
                        out.push('{');
                        render_into(e, out, MAX_DEPTH.saturating_sub(1));
                        out.push('}');
                    }
                }
            }
            out.push('"');
        }
        Lit::Unit => out.push_str("()"),
    }
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Pow => "**",
        BinOp::EqCmp => "==",
        BinOp::NeCmp => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Range => "..",
    }
}

fn unop_str(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
        UnOp::Ref => "&",
        UnOp::RefMut => "&mut ",
        UnOp::Deref => "*",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Expr, Lit};

    #[test]
    fn render_binary_comparison() {
        // b != 0
        let expr = Expr::Binary(
            BinOp::NeCmp,
            Box::new(Expr::Ident("b".to_string())),
            Box::new(Expr::Literal(Lit::Int(0))),
        );
        assert_eq!(render_expr(&expr), "b != 0");
    }

    #[test]
    fn render_call_with_field_access() {
        // len(self.items) > 0
        let callee = Expr::Ident("len".to_string());
        let arg = Expr::Field(
            Box::new(Expr::Ident("self".to_string())),
            "items".to_string(),
        );
        let call = Expr::Call(Box::new(callee), vec![arg]);
        let expr = Expr::Binary(
            BinOp::Gt,
            Box::new(call),
            Box::new(Expr::Literal(Lit::Int(0))),
        );
        assert_eq!(render_expr(&expr), "len(self.items) > 0");
    }

    #[test]
    fn render_old_in_ensures() {
        // old(balance) <= balance
        let old = Expr::Old(Box::new(Expr::Ident("balance".to_string())));
        let expr = Expr::Binary(
            BinOp::Le,
            Box::new(old),
            Box::new(Expr::Ident("balance".to_string())),
        );
        assert_eq!(render_expr(&expr), "old(balance) <= balance");
    }

    #[test]
    fn render_string_literal_escapes() {
        let expr = Expr::Literal(Lit::String("a\"b".to_string()));
        assert_eq!(render_expr(&expr), "\"a\\\"b\"");
    }

    #[test]
    fn render_truncates_pathological_expression() {
        // Nest calls deep enough to exceed MAX_RENDER_LEN.
        let mut expr = Expr::Ident("x".to_string());
        for _ in 0..80 {
            expr = Expr::Call(
                Box::new(expr),
                vec![Expr::Literal(Lit::String("padding".to_string()))],
            );
        }
        let rendered = render_expr(&expr);
        assert!(rendered.len() <= MAX_RENDER_LEN + 8);
        assert!(rendered.ends_with(" ..."));
    }
}
