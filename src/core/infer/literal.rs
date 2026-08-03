use crate::ast::{Lit, Type};
use crate::core::checker::Checker;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_literal(&self, l: &Lit) -> Type {
        match l {
            // C2 fix (audit 2026-08-03): value-aware int literal typing. An
            // integer literal outside the i32 range must infer as i64 —
            // otherwise codegen lowers it against the i32 canonical type and
            // silently truncates (9007199254740993 → 1, an L1 violation vs
            // the bytecode VM's lossless i64 literal). Matches the 0.34.6
            // one-way widening {i32→i64, i32→f64, i64→f64}: in-range literals
            // keep the i32 default (widening stays available), out-of-range
            // literals widen at the source.
            Lit::Int(v) => {
                if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    Type::Name("i32".into(), vec![])
                } else {
                    Type::Name("i64".into(), vec![])
                }
            }
            Lit::Float(_) => Type::Name("f64".into(), vec![]),
            Lit::Bool(_) => Type::Name("bool".into(), vec![]),
            Lit::String(_) => Type::Name("string".into(), vec![]),
            Lit::FString(_) => Type::Name("string".into(), vec![]),
            Lit::Unit => Type::Name("unit".into(), vec![]),
        }
    }
}
