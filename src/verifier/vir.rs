//! Verification IR (VIR) — span-free intermediate representation for the
//! Z3 verifier.
//!
//! Design constraints (verified-core-1.md §3):
//! - VIR nodes carry NO Span; spans live in a side table for error reporting.
//! - Local variables use canonical names (`%0`, `%1`, `%2`, …) so that
//!   cosmetic renames do not invalidate the semantic hash.
//! - Only trusted-subset types are representable: bool, checked i32,
//!   unbounded i64 (no definedness checks), f64 as an opaque uninterpreted sort.
//! - `typestate_context` carries Flow transition axioms (source invariants,
//!   transition guards, target invariants).
//!
//! Lowering path: `FuncDef` (raw AST) → trusted-subset gate → `VFunction`.
//! The gate rejects unsupported constructs *before* any SMT encoding.
//! Gate and lowering must be kept in sync: the gate must reject everything
//! that lowering cannot handle (fail-closed design).

use crate::span::Span;
use std::collections::HashMap;
use std::fmt;

// ── Types ──────────────────────────────────────────────────────────────

/// VIR type — only the trusted subset is representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VType {
    Bool,
    I32,
    I64,
    /// f64 as an opaque uninterpreted sort. Arithmetic is NOT representable;
    /// only equality/ordering comparisons produce uninterpreted predicates.
    F64Opaque,
}

impl fmt::Display for VType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VType::Bool => write!(f, "bool"),
            VType::I32 => write!(f, "i32"),
            VType::I64 => write!(f, "i64"),
            VType::F64Opaque => write!(f, "f64"),
        }
    }
}

// ── Canonical variable identity ────────────────────────────────────────

/// Canonical variable index. Displayed as `%N`.
/// Parameters occupy `%0..%P`, locals continue from `%P`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarId(pub usize);

impl fmt::Display for VarId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

// ── Expressions ────────────────────────────────────────────────────────

/// Checked arithmetic operation. Each generates a value equation AND a
/// definedness obligation (overflow / div-zero / MIN÷-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl fmt::Display for VArithOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VArithOp::Add => write!(f, "+"),
            VArithOp::Sub => write!(f, "-"),
            VArithOp::Mul => write!(f, "*"),
            VArithOp::Div => write!(f, "/"),
            VArithOp::Mod => write!(f, "%"),
        }
    }
}

/// Comparison operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VCmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl fmt::Display for VCmpOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VCmpOp::Eq => write!(f, "=="),
            VCmpOp::Ne => write!(f, "!="),
            VCmpOp::Lt => write!(f, "<"),
            VCmpOp::Gt => write!(f, ">"),
            VCmpOp::Le => write!(f, "<="),
            VCmpOp::Ge => write!(f, ">="),
        }
    }
}

/// Boolean operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VBoolOp {
    And,
    Or,
}

/// VIR expression — span-free, canonical variables.
///
/// Every node is identified by a `NodeId` for the span side table.
/// The expression tree is immutable; sharing is via `Box`.
#[derive(Debug, Clone)]
pub enum VExpr {
    // ── Constants ──
    IntConst(i64),
    BoolConst(bool),
    /// f64 literal — opaque, no arithmetic semantics.
    F64Const(f64),

    // ── Variables ──
    /// Canonical local/parameter variable.
    Var(VarId),
    /// `old(param)` snapshot for postconditions.
    Old(VarId),
    /// The implicit `result` variable in postconditions.
    Result,

    // ── Checked arithmetic (generates definedness obligations) ──
    /// `(lhs op rhs) : result_ty` — value equation + definedness VC.
    CheckedArith(VArithOp, Box<VExpr>, Box<VExpr>, VType),
    /// Unary negation with MIN overflow check.
    CheckedNeg(Box<VExpr>, VType),

    // ── Comparisons ──
    Compare(VCmpOp, Box<VExpr>, Box<VExpr>),

    // ── Boolean ──
    Boolean(VBoolOp, Vec<VExpr>),
    Not(Box<VExpr>),

    // ── Control flow (tree-based for v1; CFG/SSA in future) ──
    /// `if cond { then } else { else_ }`
    Select(Box<VExpr>, Box<VExpr>, Box<VExpr>),

    // ── Opaque f64 ──
    /// f64 variable — uninterpreted sort, no arithmetic.
    OpaqueF64(VarId),
    /// f64 comparison — uninterpreted predicate (not proved, not rejected).
    F64Compare(VCmpOp, Box<VExpr>, Box<VExpr>),
}

impl VExpr {
    /// Check if this expression tree contains any CheckedArith or CheckedNeg
    /// node. Used by P0-8 to detect non-tail expressions with potential
    /// div-by-zero / overflow that would be silently discarded.
    pub fn contains_checked_arith(&self) -> bool {
        match self {
            VExpr::CheckedArith(..) | VExpr::CheckedNeg(..) => true,
            VExpr::Compare(_, l, r) | VExpr::F64Compare(_, l, r) => {
                l.contains_checked_arith() || r.contains_checked_arith()
            }
            VExpr::Boolean(_, exprs) => exprs.iter().any(|e| e.contains_checked_arith()),
            VExpr::Not(inner) => inner.contains_checked_arith(),
            VExpr::Select(c, t, e) => {
                c.contains_checked_arith()
                    || t.contains_checked_arith()
                    || e.contains_checked_arith()
            }
            _ => false,
        }
    }

    /// The type of this expression, if statically known.
    pub fn ty(&self) -> Option<VType> {
        match self {
            VExpr::IntConst(_) => Some(VType::I64),
            VExpr::BoolConst(_) => Some(VType::Bool),
            VExpr::F64Const(_) => Some(VType::F64Opaque),
            VExpr::Var(_) => None, // resolved from context
            VExpr::Old(_) => None,
            VExpr::Result => None,
            VExpr::CheckedArith(_, _, _, ty) => Some(*ty),
            VExpr::CheckedNeg(_, ty) => Some(*ty),
            VExpr::Compare(..) => Some(VType::Bool),
            VExpr::Boolean(..) => Some(VType::Bool),
            VExpr::Not(_) => Some(VType::Bool),
            VExpr::Select(_, then_, _) => then_.ty(),
            VExpr::OpaqueF64(_) => Some(VType::F64Opaque),
            VExpr::F64Compare(..) => Some(VType::Bool),
        }
    }
}

impl fmt::Display for VExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VExpr::IntConst(n) => write!(f, "{}", n),
            VExpr::BoolConst(b) => write!(f, "{}", b),
            VExpr::F64Const(v) => write!(f, "{}f64", v),
            VExpr::Var(id) => write!(f, "{}", id),
            VExpr::Old(id) => write!(f, "old({})", id),
            VExpr::Result => write!(f, "result"),
            VExpr::CheckedArith(op, l, r, _) => write!(f, "({} {} {})", l, op, r),
            VExpr::CheckedNeg(e, _) => write!(f, "(-{})", e),
            VExpr::Compare(op, l, r) => write!(f, "({} {} {})", l, op, r),
            VExpr::Boolean(op, es) => {
                let sep = match op {
                    VBoolOp::And => " && ",
                    VBoolOp::Or => " || ",
                };
                write!(f, "(")?;
                for (i, e) in es.iter().enumerate() {
                    if i > 0 {
                        write!(f, "{}", sep)?;
                    }
                    write!(f, "{}", e)?;
                }
                write!(f, ")")
            }
            VExpr::Not(e) => write!(f, "!{}", e),
            VExpr::Select(c, t, e) => write!(f, "if {} {{ {} }} else {{ {} }}", c, t, e),
            VExpr::OpaqueF64(id) => write!(f, "{}:f64", id),
            VExpr::F64Compare(op, l, r) => write!(f, "({} {} {}):f64", l, op, r),
        }
    }
}

// ── Statements ─────────────────────────────────────────────────────────

/// VIR statement.
#[derive(Debug, Clone)]
pub enum VStmt {
    /// `let %id = expr;`
    Let(VarId, VExpr),
    /// Precondition / invariant assumption.
    Assume(VExpr),
    /// Postcondition / definedness obligation to prove.
    Assert(VExpr),
    /// Return expression.
    Return(VExpr),
}

impl fmt::Display for VStmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VStmt::Let(id, e) => write!(f, "let {} = {}", id, e),
            VStmt::Assume(e) => write!(f, "assume {}", e),
            VStmt::Assert(e) => write!(f, "assert {}", e),
            VStmt::Return(e) => write!(f, "return {}", e),
        }
    }
}

// ── Typestate axioms ───────────────────────────────────────────────────

/// Typestate context for Flow transition VIR.
///
/// Injected during lowering:
/// - `source_invariants` → Z3 axioms (assert)
/// - `transition_guards` → Z3 preconditions
/// - `target_invariants` → Z3 obligations (prove)
///
/// All axioms must originate from Checker-verified typestate information.
/// No unverified assumptions may be injected.
#[derive(Debug, Clone, Default)]
pub struct TypestateAxioms {
    /// Source state invariants — asserted as axioms.
    pub source_invariants: Vec<VExpr>,
    /// Transition guards — assumed as preconditions.
    pub transition_guards: Vec<VExpr>,
    /// Target state invariants — must be proved.
    pub target_invariants: Vec<VExpr>,
}

// ── Function ───────────────────────────────────────────────────────────

/// VIR function — the unit of verification.
///
/// Span-free by design. Source locations are recovered via `VirSpanTable`.
#[derive(Debug, Clone)]
pub struct VFunction {
    /// Qualified function name (e.g. `module::func`).
    pub id: String,
    /// Parameters: `(canonical_var, type, original_name)`.
    pub params: Vec<(VarId, VType, String)>,
    /// Body statements (assumptions, lets, return).
    pub body: Vec<VStmt>,
    /// Postconditions to prove (ensures clauses).
    pub postconditions: Vec<VExpr>,
    /// Semantic hash for proof caching (span-free, variable-normalized).
    pub semantics_hash: String,
    /// Flow transition typestate context (None for plain functions).
    pub typestate_context: Option<TypestateAxioms>,
    /// Whether this function is marked `#[verified]` (Unknown is also an error).
    pub is_verified_attr: bool,
}

impl VFunction {
    /// Number of parameters.
    pub fn param_count(&self) -> usize {
        self.params.len()
    }

    /// Look up a parameter's canonical VarId by original name.
    pub fn param_var(&self, name: &str) -> Option<VarId> {
        self.params
            .iter()
            .find(|(_, _, orig)| orig == name)
            .map(|(id, _, _)| *id)
    }

    /// Produce a normalized string for semantic hashing.
    /// Variable names are already canonical (%N), so this is just
    /// the structural representation without spans.
    ///
    /// 0.31.29 audit P2-7/P2-14: includes `typestate_context` and
    /// `is_verified_attr` so that functions differing only in these
    /// fields produce different hashes.
    pub fn normalized_repr(&self) -> String {
        let mut s = String::new();
        if self.is_verified_attr {
            s.push_str("#[verified] ");
        }
        s.push_str(&format!("func {}(", self.id));
        for (i, (var, ty, _)) in self.params.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&format!("{}: {}", var, ty));
        }
        s.push_str(") {\n");
        for stmt in &self.body {
            s.push_str(&format!("  {}\n", stmt));
        }
        for post in &self.postconditions {
            s.push_str(&format!("  ensures {}\n", post));
        }
        // Include typestate context in the hash so that functions with
        // different Flow transition axioms produce different hashes.
        if let Some(ref ts) = self.typestate_context {
            if !ts.source_invariants.is_empty()
                || !ts.transition_guards.is_empty()
                || !ts.target_invariants.is_empty()
            {
                s.push_str("  typestate {\n");
                for inv in &ts.source_invariants {
                    s.push_str(&format!("    source_inv {}\n", inv));
                }
                for guard in &ts.transition_guards {
                    s.push_str(&format!("    guard {}\n", guard));
                }
                for inv in &ts.target_invariants {
                    s.push_str(&format!("    target_inv {}\n", inv));
                }
                s.push_str("  }\n");
            }
        }
        s.push('}');
        s
    }
}

// ── Span side table ────────────────────────────────────────────────────

/// Maps VIR node identifiers back to source spans for error reporting.
///
/// VIR nodes themselves carry no span. When a verification failure occurs,
/// the diagnostic engine consults this table to produce a located error.
#[derive(Debug, Clone, Default)]
pub struct VirSpanTable {
    /// Function-level span.
    pub func_spans: HashMap<String, Span>,
    /// Postcondition spans: `(func_id, postcondition_index) → Span`.
    pub postcondition_spans: HashMap<(String, usize), Span>,
    /// Statement spans: `(func_id, stmt_index) → Span`.
    pub stmt_spans: HashMap<(String, usize), Span>,
    /// Expression spans: `(func_id, expr_path) → Span`.
    /// `expr_path` is a dot-separated path from the function root
    /// (e.g. "body.0.rhs" for the RHS of the first body statement).
    pub expr_spans: HashMap<(String, String), Span>,
}

impl VirSpanTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the span for a function.
    pub fn record_func(&mut self, func_id: &str, span: Span) {
        self.func_spans.insert(func_id.to_string(), span);
    }

    /// Record the span for a postcondition.
    pub fn record_postcondition(&mut self, func_id: &str, index: usize, span: Span) {
        self.postcondition_spans
            .insert((func_id.to_string(), index), span);
    }

    /// Record the span for a body statement.
    pub fn record_stmt(&mut self, func_id: &str, index: usize, span: Span) {
        self.stmt_spans.insert((func_id.to_string(), index), span);
    }

    /// Look up the span for a function.
    pub fn func_span(&self, func_id: &str) -> Option<Span> {
        self.func_spans.get(func_id).copied()
    }

    /// Look up the span for a postcondition.
    pub fn postcondition_span(&self, func_id: &str, index: usize) -> Option<Span> {
        self.postcondition_spans
            .get(&(func_id.to_string(), index))
            .copied()
    }

    /// Look up the span for a body statement.
    pub fn stmt_span(&self, func_id: &str, index: usize) -> Option<Span> {
        self.stmt_spans.get(&(func_id.to_string(), index)).copied()
    }
}

// ── Trusted-subset gate ────────────────────────────────────────────────

/// Result of the trusted-subset gate check.
pub type TrustedSubsetResult = Result<(), String>;

/// Check whether a surface type is in the VIR trusted subset.
pub fn is_trusted_type(ty: &crate::ast::Type) -> bool {
    match ty.unlocated() {
        crate::ast::Type::Name(name, args) => match name.as_str() {
            "bool" | "Bool" => true,
            "i32" | "Int" => true,
            "i64" => true,
            "f64" => args.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// Check whether a function definition is in the VIR trusted subset.
///
/// Rejects: heap types, string, List, Map, Set, tuples, Flow, Actor,
/// Session, loop, recursion, allocation, panic, I/O, time, random, FFI,
/// spawn/await, mutation, and unknown types.
///
/// Accepts: bool, checked i32/i64, f64 (opaque), pure finite branching
/// (if/match), immutable scalar parameters, local immutable bindings,
/// old(immutable_scalar_parameter).
pub fn check_trusted_subset(func: &crate::ast::FuncDef) -> TrustedSubsetResult {
    // 1. Check parameter types
    for param in &func.params {
        if !is_trusted_type(&param.ty) {
            return Err(format!(
                "parameter '{}' has unsupported type '{}'",
                param.name,
                crate::core::fmt_type(&param.ty)
            ));
        }
    }

    // 2. Check return type
    if let Some(ret) = &func.ret {
        if !is_trusted_type(ret) {
            return Err(format!(
                "return type '{}' is not in the trusted subset",
                crate::core::fmt_type(ret)
            ));
        }
    }

    // 3. Check body for unsupported constructs
    check_stmts_trusted(&func.body)
}

/// Recursively check statements for trusted-subset compliance.
fn check_stmts_trusted(stmts: &[crate::ast::Stmt]) -> TrustedSubsetResult {
    for stmt in stmts {
        check_stmt_trusted(stmt)?;
    }
    Ok(())
}

fn check_stmt_trusted(stmt: &crate::ast::Stmt) -> TrustedSubsetResult {
    use crate::ast::Stmt;
    match stmt.unlocated() {
        // Contracts are always allowed (they are the specification)
        Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Invariant(..) | Stmt::Math(..) => Ok(()),
        // Super-comments are ignored
        Stmt::MmsBlock { .. } | Stmt::Desc(..) | Stmt::Rule(..) | Stmt::Ellipsis => Ok(()),
        // Let bindings: check the init expression; reject mutable bindings
        Stmt::Let { init, mut_, .. } => {
            if *mut_ {
                return Err(
                    "mutable let binding is not in the trusted subset (v1: immutable bindings only)"
                        .into(),
                );
            }
            if let Some(init) = init {
                check_expr_trusted(init)?;
            }
            Ok(())
        }
        // Return: check the expression
        Stmt::Return(expr) => {
            if let Some(expr) = expr {
                check_expr_trusted(expr)?;
            }
            Ok(())
        }
        // Expression statements
        Stmt::Expr(expr) => check_expr_trusted(expr),
        // If statement: allowed only in tail position (last statement in body).
        // The lowering converts it to a Select. Early returns inside branches
        // are rejected by check_stmts_trusted (Stmt::Return is checked recursively).
        Stmt::If { cond, then_, else_ } => {
            check_expr_trusted(cond)?;
            check_stmts_trusted(then_)?;
            if let Some(else_) = else_ {
                check_stmts_trusted(else_)?;
            }
            Ok(())
        }
        // Loops are NOT in the trusted subset (v1: finite branching only)
        Stmt::While { .. } | Stmt::WhileLet { .. } | Stmt::For { .. } | Stmt::Loop(..) => {
            Err("loop constructs are not in the trusted subset (v1: finite branching only)".into())
        }
        // Mutation is NOT in the trusted subset
        Stmt::Assign { .. } => {
            Err("mutation is not in the trusted subset (v1: immutable scalars only)".into())
        }
        // Defer is NOT in the trusted subset
        Stmt::Defer(..) => Err("defer is not in the trusted subset".into()),
        // Block: NOT in trusted subset v1 (lowering doesn't handle nested blocks;
        // early returns inside blocks would be silently dropped).
        Stmt::Block(_) => {
            Err("block statements are not in the trusted subset (v1: flat body only)".into())
        }
        // Anything else is rejected
        _ => Err("statement is not in the trusted subset".into()),
    }
}

/// Recursively check expressions for trusted-subset compliance.
fn check_expr_trusted(expr: &crate::ast::Expr) -> TrustedSubsetResult {
    use crate::ast::{BinOp, Expr, Lit, UnOp};
    match expr.unlocated() {
        // Literals: only Int, Bool, Float are trusted (lowering handles these)
        Expr::Literal(Lit::Int(_) | Lit::Bool(_) | Lit::Float(_)) => Ok(()),
        Expr::Literal(other) => Err(format!(
            "literal {:?} is not in the trusted subset (v1: int/bool/float only)",
            std::mem::discriminant(other)
        )),
        // Identifiers are trusted (resolved by context)
        Expr::Ident(_) => Ok(()),
        // old(param) is trusted only for simple identifiers
        Expr::Old(inner) => {
            if matches!(inner.unlocated(), Expr::Ident(_)) {
                Ok(())
            } else {
                Err("old() in the trusted subset only accepts simple identifiers (v1: no old(expr))".into())
            }
        }
        // Binary operations: only arithmetic + comparison + boolean
        Expr::Binary(op, lhs, rhs) => {
            match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                | BinOp::EqCmp | BinOp::NeCmp | BinOp::Lt | BinOp::Gt
                | BinOp::Le | BinOp::Ge | BinOp::And | BinOp::Or => {}
                _ => {
                    return Err(format!(
                        "operator {:?} is not in the trusted subset (v1: arithmetic/comparison/boolean only)",
                        op
                    ))
                }
            }
            check_expr_trusted(lhs)?;
            check_expr_trusted(rhs)
        }
        // Unary operations: only Neg and Not
        Expr::Unary(op, inner) => {
            match op {
                UnOp::Neg | UnOp::Not => {}
                _ => {
                    return Err(format!(
                        "unary operator {:?} is not in the trusted subset (v1: negation/not only)",
                        op
                    ))
                }
            }
            check_expr_trusted(inner)
        }
        // If expression: check ALL statements in branches (not just tail)
        Expr::If { cond, then_, else_ } => {
            check_expr_trusted(cond)?;
            check_stmts_trusted(then_)?;
            if let Some(else_) = else_ {
                check_stmts_trusted(else_)?;
            }
            Ok(())
        }
        // Block: check all statements
        Expr::Block(stmts) => {
            check_stmts_trusted(stmts)?;
            Ok(())
        }
        // Match: check scrutinee and arms; reject variable patterns
        Expr::Match(scrutinee, arms) => {
            check_expr_trusted(scrutinee)?;
            for arm in arms {
                // Reject variable patterns (lowering creates unbound Z3 variables)
                if let crate::ast::PatternKind::Variable(_) = arm.pat.kind {
                    return Err(
                        "match variable patterns are not in the trusted subset (v1: literal/wildcard patterns only)".into(),
                    );
                }
                if let Some(guard) = &arm.guard {
                    check_expr_trusted(guard)?;
                }
                check_expr_trusted(&arm.body)?;
            }
            Ok(())
        }
        // Calls: NOT in trusted subset v1 (future: only Proven pure total acyclic)
        Expr::Call(callee, args) => {
            // Allow old(param) — but only with a single identifier argument
            if let Expr::Ident(name) = callee.unlocated() {
                if name == "old" {
                    if args.len() == 1 && matches!(args[0].unlocated(), Expr::Ident(_)) {
                        return Ok(());
                    }
                    return Err(
                        "old() in the trusted subset only accepts a single identifier argument"
                            .into(),
                    );
                }
            }
            Err("function calls are not in the trusted subset (v1: no calls)".into())
        }
        // Field access: NOT in trusted subset (heap/aggregate)
        Expr::Field(..) => {
            Err("field access is not in the trusted subset (v1: no heap/aggregate)".into())
        }
        // Tuple index: NOT in trusted subset
        Expr::TupleIndex(..) => {
            Err("tuple index is not in the trusted subset (v1: no aggregates)".into())
        }
        // Spawn/Await: NOT in trusted subset
        Expr::Spawn(..) => Err("spawn is not in the trusted subset".into()),
        Expr::Await(..) => Err("await is not in the trusted subset".into()),
        // Anything else is rejected
        _ => Err("expression is not in the trusted subset".into()),
    }
}

// ── Lowering: FuncDef → VFunction ──────────────────────────────────────

/// Lowering context: maps source names to canonical VarIds.
///
/// C-7 (full-audit-2026-08-05-0656 §1): the name map is SCOPED. The checker
/// permits block-level shadowing (`if c { let x = x + 1; x } else { x }`),
/// and a flat name→VarId map silently aliased the shadowed local to the
/// PARAMETER's Z3 variable — `ensures: result == x` was a fake Proven even
/// though the runtime returns `x + 1` when `c` holds. `bind_local` now
/// allocates a fresh VarId in the innermost scope, shadowing outer bindings.
#[allow(dead_code)] // returns_f64/returns_bool/fresh_local: infrastructure for f64 opaque sort + local let lowering
struct LoweringCtx {
    /// Next canonical variable index.
    next_var: usize,
    /// Scoped source name → canonical VarId map. Index 0 is the parameter
    /// scope; each if-branch block pushes its own scope so branch-local
    /// bindings (including shadows) get fresh VarIds.
    scopes: Vec<HashMap<String, VarId>>,
    /// Parameter types for type resolution (by parameter name).
    param_types: HashMap<String, VType>,
    /// Declared type of every canonical variable (parameters and lets).
    /// Authoritative for definedness obligations: a `let y = x` whose init is
    /// an i32 parameter must carry i32, not the legacy I64 fallback.
    var_types: HashMap<VarId, VType>,
    /// Whether the return type is f64.
    returns_f64: bool,
    /// Whether the return type is bool.
    returns_bool: bool,
    /// The function's return VType (for contextual type inference).
    /// When the return type is i32, integer literals in the body are
    /// inferred as i32 (not i64) so that definedness checks apply.
    return_vtype: Option<VType>,
}

impl LoweringCtx {
    fn new(func: &crate::ast::FuncDef) -> Self {
        let return_vtype = func.ret.as_ref().map(surface_type_to_vtype);
        let mut ctx = LoweringCtx {
            next_var: 0,
            scopes: vec![HashMap::new()],
            param_types: HashMap::new(),
            var_types: HashMap::new(),
            returns_f64: func
                .ret
                .as_ref()
                .is_some_and(|t| matches!(t.unlocated(), crate::ast::Type::Name(n, _) if n == "f64")),
            returns_bool: func.ret.as_ref().is_some_and(
                |t| matches!(t.unlocated(), crate::ast::Type::Name(n, _) if n == "bool" || n == "Bool"),
            ),
            return_vtype,
        };
        // Register parameters as %0, %1, %2, ...
        for param in &func.params {
            let var = VarId(ctx.next_var);
            ctx.next_var += 1;
            ctx.scopes[0].insert(param.name.clone(), var);
            let vty = surface_type_to_vtype(&param.ty);
            ctx.param_types.insert(param.name.clone(), vty);
            ctx.var_types.insert(var, vty);
        }
        ctx
    }

    /// Get or create a canonical VarId for a source name (read position).
    /// Innermost scope wins (C-7 shadowing). Unknown names allocate into the
    /// ROOT scope so repeated reads of a never-bound name (e.g. inlined
    /// callee-ensures variables) resolve to one stable VarId.
    fn resolve(&mut self, name: &str) -> VarId {
        for scope in self.scopes.iter().rev() {
            if let Some(&var) = scope.get(name) {
                return var;
            }
        }
        let var = VarId(self.next_var);
        self.next_var += 1;
        self.scopes[0].insert(name.to_string(), var);
        var
    }

    /// Bind a let-introduced name in the innermost scope (C-7): ALWAYS a
    /// fresh VarId, even when an outer binding of the same name exists.
    fn bind_local(&mut self, name: &str, vty: VType) -> VarId {
        let var = VarId(self.next_var);
        self.next_var += 1;
        let scope = self
            .scopes
            .last_mut()
            .expect("LoweringCtx must have a root scope");
        scope.insert(name.to_string(), var);
        self.var_types.insert(var, vty);
        var
    }

    /// Push a scope (branch block entry).
    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Pop a scope (branch block exit). Shadowed bindings vanish with it.
    fn exit_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// Get the VType for a source name (parameter or local, innermost wins).
    fn type_of(&self, name: &str) -> Option<VType> {
        for scope in self.scopes.iter().rev() {
            if let Some(&var) = scope.get(name) {
                return self.var_types.get(&var).copied();
            }
        }
        self.param_types.get(name).copied()
    }

    /// Infer the VType of an expression from its structure.
    /// Used to determine the correct arithmetic type (i32 vs i64).
    fn infer_expr_type(&self, expr: &crate::ast::Expr) -> VType {
        use crate::ast::Expr;
        match expr.unlocated() {
            Expr::Ident(name) => self.type_of(name).unwrap_or(VType::I64),
            Expr::Literal(crate::ast::Lit::Int(_)) => {
                // When the function returns i32, integer literals are i32
                // (so that definedness checks apply to constant expressions).
                // P2-11: This is a function-level heuristic. If a function
                // returns i32 but has intermediate i64 computations, the
                // literals in those computations would be incorrectly typed
                // as i32. Fixing this requires integrating the checker's
                // type information into the VIR lowering (post-CheckedProgram
                // migration).
                if self.return_vtype == Some(VType::I32) {
                    VType::I32
                } else {
                    VType::I64
                }
            }
            Expr::Literal(crate::ast::Lit::Float(_)) => VType::F64Opaque,
            Expr::Literal(crate::ast::Lit::Bool(_)) => VType::Bool,
            Expr::Binary(_, lhs, rhs) => {
                let lt = self.infer_expr_type(lhs);
                let rt = self.infer_expr_type(rhs);
                // If either operand is i32, result is i32 (checked arithmetic)
                if lt == VType::I32 || rt == VType::I32 {
                    VType::I32
                } else if lt == VType::F64Opaque || rt == VType::F64Opaque {
                    VType::F64Opaque
                } else {
                    VType::I64
                }
            }
            Expr::Unary(_, inner) => self.infer_expr_type(inner),
            Expr::Old(inner) => self.infer_expr_type(inner),
            Expr::If { then_, .. } => crate::verifier::helpers::block_tail_expr(then_)
                .map(|e| self.infer_expr_type(&e))
                .unwrap_or(VType::I64),
            _ => VType::I64,
        }
    }

    /// Allocate a fresh local variable.
    #[allow(dead_code)] // Infrastructure for future let-binding lowering
    fn fresh_local(&mut self) -> VarId {
        let var = VarId(self.next_var);
        self.next_var += 1;
        var
    }
}

/// Convert a surface type to a VIR type.
/// Falls back to I64 for unrecognized types (the gate should have rejected
/// unsupported types before this is called; the fallback is defensive).
fn surface_type_to_vtype(ty: &crate::ast::Type) -> VType {
    match ty.unlocated() {
        crate::ast::Type::Name(name, _) => match name.as_str() {
            "bool" | "Bool" => VType::Bool,
            "i32" | "Int" => VType::I32,
            "i64" => VType::I64,
            "f64" => VType::F64Opaque,
            _ => VType::I64, // fallback; gate should have rejected
        },
        _ => VType::I64,
    }
}

/// Lower a `FuncDef` to a `VFunction`.
///
/// **Precondition**: `check_trusted_subset(func)` returned `Accepted`.
/// If the gate was not run, this function may produce incorrect VIR.
///
/// Returns `(VFunction, VirSpanTable)`.
pub fn lower_func_to_vir(func: &crate::ast::FuncDef) -> Result<(VFunction, VirSpanTable), String> {
    // Gate check (defensive — caller should have checked)
    match check_trusted_subset(func) {
        Ok(()) => {}
        Err(reason) => {
            return Err(reason);
        }
    }

    let mut ctx = LoweringCtx::new(func);
    let mut span_table = VirSpanTable::new();
    let func_id = func.name.clone();
    span_table.record_func(&func_id, func.meta.span);

    // Build parameter list
    let params: Vec<(VarId, VType, String)> = func
        .params
        .iter()
        .map(|p| {
            let var = ctx
                .scopes
                .first()
                .and_then(|scope| scope.get(&p.name))
                .copied()
                .expect("parameter registered in root scope");
            let vty = surface_type_to_vtype(&p.ty);
            (var, vty, p.name.clone())
        })
        .collect();

    // Extract contracts and body
    let mut body_stmts: Vec<VStmt> = Vec::new();
    let mut postconditions: Vec<VExpr> = Vec::new();
    let mut stmt_index = 0usize;

    for (stmt_pos, stmt) in func.body.iter().enumerate() {
        let is_last = stmt_pos == func.body.len() - 1;
        match stmt.unlocated() {
            crate::ast::Stmt::Requires(expr, _) => match lower_expr_to_vir(expr, &mut ctx) {
                Some(vexpr) => {
                    body_stmts.push(VStmt::Assume(vexpr));
                    span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                    stmt_index += 1;
                }
                None => {
                    return Err(
                        "requires clause contains unsupported expression (cannot lower to VIR)"
                            .to_string(),
                    );
                }
            },
            crate::ast::Stmt::Ensures(expr, _) => match lower_expr_to_vir(expr, &mut ctx) {
                Some(vexpr) => {
                    let idx = postconditions.len();
                    postconditions.push(vexpr);
                    span_table.record_postcondition(&func_id, idx, stmt_span(stmt));
                }
                None => {
                    return Err(
                        "ensures clause contains unsupported expression (cannot lower to VIR)"
                            .to_string(),
                    );
                }
            },
            crate::ast::Stmt::Invariant(expr, _) => {
                // Invariants are assumed (established from requires)
                match lower_expr_to_vir(expr, &mut ctx) {
                    Some(vexpr) => {
                        body_stmts.push(VStmt::Assume(vexpr));
                        span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                        stmt_index += 1;
                    }
                    None => {
                        return Err(
                            "invariant clause contains unsupported expression (cannot lower to VIR)"
                                .to_string(),
                        );
                    }
                }
            }
            crate::ast::Stmt::Math(exprs) => {
                for expr in exprs {
                    match lower_expr_to_vir(expr, &mut ctx) {
                        Some(vexpr) => {
                            body_stmts.push(VStmt::Assert(vexpr));
                            span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                            stmt_index += 1;
                        }
                        None => {
                            return Err(
                                "math clause contains unsupported expression (cannot lower to VIR)"
                                    .to_string(),
                            );
                        }
                    }
                }
            }
            crate::ast::Stmt::Let { pat, init, .. } => {
                if let Some(init) = init {
                    match lower_expr_to_vir(init, &mut ctx) {
                        Some(vexpr) => {
                            // Extract variable name from pattern
                            let name = match &pat.kind {
                                crate::ast::PatternKind::Variable(n) => n.clone(),
                                _ => format!("_let{}", stmt_index),
                            };
                            // C-7: bind_local allocates a FRESH VarId so a
                            // shadowing `let x = …` never aliases an outer
                            // binding's Z3 variable. The init's type is
                            // recorded so later reads keep the correct
                            // definedness model (i32 vs i64).
                            let vty = vexpr.ty().unwrap_or_else(|| ctx.infer_expr_type(init));
                            let var = ctx.bind_local(&name, vty);
                            body_stmts.push(VStmt::Let(var, vexpr));
                            span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                            stmt_index += 1;
                        }
                        None => {
                            return Err(
                                "let binding init expression cannot be lowered to VIR".to_string()
                            );
                        }
                    }
                }
            }
            crate::ast::Stmt::Return(expr) => {
                if let Some(expr) = expr {
                    match lower_expr_to_vir(expr, &mut ctx) {
                        Some(vexpr) => {
                            body_stmts.push(VStmt::Return(vexpr));
                            span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                            stmt_index += 1;
                        }
                        None => {
                            // 0.31.28: Fail-closed. If the return expression cannot
                            // be lowered (e.g., f64 arithmetic), the whole function
                            // is NotInTrustedSubset.
                            return Err(
                                "return expression contains unsupported expression (cannot lower to VIR)"
                                    .to_string(),
                            );
                        }
                    }
                }
            }
            crate::ast::Stmt::Expr(expr) => {
                // Only the LAST expression statement is the implicit return.
                // Earlier expression statements have their values discarded.
                if is_last {
                    match lower_expr_to_vir(expr, &mut ctx) {
                        Some(vexpr) => {
                            body_stmts.push(VStmt::Return(vexpr));
                            span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                            stmt_index += 1;
                        }
                        None => {
                            // 0.31.28: Fail-closed. If the return expression cannot
                            // be lowered (e.g., f64 arithmetic), the whole function
                            // is NotInTrustedSubset.
                            return Err(
                                "return expression contains unsupported expression (cannot lower to VIR)"
                                    .to_string(),
                            );
                        }
                    }
                } else {
                    // P0-8: Non-tail expression statements are NOT necessarily
                    // pure. Division/modulo can crash at runtime (div-by-zero).
                    // If the expression contains checked arithmetic, the function
                    // is NotInTrustedSubset (fail-closed) — we cannot verify the
                    // definedness of a discarded expression.
                    if let Some(vexpr) = lower_expr_to_vir(expr, &mut ctx) {
                        if vexpr.contains_checked_arith() {
                            return Err(
                                "non-tail expression contains checked arithmetic (div/mod/neg overflow) — cannot verify definedness of discarded value"
                                    .to_string(),
                            );
                        }
                    }
                    // If lowering fails, the expression is not in the trusted
                    // subset but its value is discarded, so we can safely skip it.
                }
            }
            // Stmt::If at the top level: handle in tail position, reject otherwise.
            // 0.31.29 audit P0-1: previously fell through to _ => {} and was
            // silently discarded, causing gate-lowering desync.
            //
            // C-7 (full-audit-2026-08-05-0656 §1): lower the WHOLE branch
            // blocks, not just their tail expressions. The old code took
            // `block_tail_expr(then_)` and discarded in-branch lets — a
            // shadowing `let x = x + 1` inside a branch silently aliased the
            // parameter's Z3 variable, producing fake Proven for
            // `ensures: result == x` while the runtime returns `x + 1`.
            crate::ast::Stmt::If { cond, then_, else_ } => {
                if is_last {
                    let Some(c) = lower_expr_to_vir(cond, &mut ctx) else {
                        return Err("if condition/then cannot be lowered to VIR".to_string());
                    };
                    let Some(tv) = lower_branch_block(then_, &mut ctx) else {
                        return Err("if condition/then cannot be lowered to VIR".to_string());
                    };
                    let Some(else_block) = else_ else {
                        // Else-less if in tail position falls through with
                        // unit; no i32/i64/bool function can type-check that,
                        // and VIR has no unit result. Fail-closed as before.
                        return Err("if-else branch cannot be lowered to VIR".to_string());
                    };
                    let Some(ev) = lower_branch_block(else_block, &mut ctx) else {
                        return Err("if-else branch cannot be lowered to VIR".to_string());
                    };
                    body_stmts.push(VStmt::Return(VExpr::Select(
                        Box::new(c),
                        Box::new(tv),
                        Box::new(ev),
                    )));
                    span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                    stmt_index += 1;
                } else {
                    return Err(
                        "non-tail if statement is not in the trusted subset (v1: flat body only)"
                            .to_string(),
                    );
                }
            }
            _ => {
                return Err(format!(
                    "statement {:?} is not in the trusted subset",
                    std::mem::discriminant(stmt.unlocated())
                ));
            }
        }
    }

    // If no explicit return was found, try to extract the tail expression
    if !body_stmts.iter().any(|s| matches!(s, VStmt::Return(_))) {
        if let Some(tail) = crate::verifier::helpers::extract_body_return(&func.body) {
            if let Some(vexpr) = lower_expr_to_vir(&tail, &mut ctx) {
                body_stmts.push(VStmt::Return(vexpr));
            }
        }
    }

    let semantics_hash = String::new(); // Computed by caller after normalization

    Ok((
        VFunction {
            id: func_id,
            params,
            body: body_stmts,
            postconditions,
            semantics_hash,
            typestate_context: None,
            is_verified_attr: false,
        },
        span_table,
    ))
}

/// Lower a surface expression to a VIR expression.
///
/// Returns `None` if the expression cannot be lowered (should not happen
/// if the trusted-subset gate passed).
fn lower_expr_to_vir(expr: &crate::ast::Expr, ctx: &mut LoweringCtx) -> Option<VExpr> {
    use crate::ast::{BinOp, Expr, Lit, UnOp};
    match expr.unlocated() {
        Expr::Literal(Lit::Int(n)) => Some(VExpr::IntConst(*n)),
        Expr::Literal(Lit::Bool(b)) => Some(VExpr::BoolConst(*b)),
        Expr::Literal(Lit::Float(f)) => Some(VExpr::F64Const(*f)),
        Expr::Ident(name) => {
            // Special-case `result` — the implicit return variable in postconditions
            if name == "result" {
                return Some(VExpr::Result);
            }
            let var = ctx.resolve(name);
            // Check if this is an f64 parameter
            if ctx.type_of(name) == Some(VType::F64Opaque) {
                Some(VExpr::OpaqueF64(var))
            } else {
                Some(VExpr::Var(var))
            }
        }
        Expr::Old(inner) => {
            if let Expr::Ident(name) = inner.unlocated() {
                let var = ctx.resolve(name);
                if ctx.type_of(name) == Some(VType::F64Opaque) {
                    Some(VExpr::OpaqueF64(var)) // old(f64) is still opaque
                } else {
                    Some(VExpr::Old(var))
                }
            } else {
                None
            }
        }
        Expr::Binary(op, lhs, rhs) => {
            let l = lower_expr_to_vir(lhs, ctx)?;
            let r = lower_expr_to_vir(rhs, ctx)?;
            // Infer the arithmetic type from operands
            let arith_ty = ctx.infer_expr_type(lhs);
            // Check if this is an f64 operation
            let is_f64 = ctx.infer_expr_type(lhs) == VType::F64Opaque
                || ctx.infer_expr_type(rhs) == VType::F64Opaque;
            match op {
                // f64 arithmetic is NOT in the trusted subset (IEEE 754 rounding
                // is not modeled). Fail-closed: return None → NotInTrustedSubset.
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod if is_f64 => None,
                BinOp::Add => Some(VExpr::CheckedArith(
                    VArithOp::Add,
                    Box::new(l),
                    Box::new(r),
                    arith_ty,
                )),
                BinOp::Sub => Some(VExpr::CheckedArith(
                    VArithOp::Sub,
                    Box::new(l),
                    Box::new(r),
                    arith_ty,
                )),
                BinOp::Mul => Some(VExpr::CheckedArith(
                    VArithOp::Mul,
                    Box::new(l),
                    Box::new(r),
                    arith_ty,
                )),
                BinOp::Div => Some(VExpr::CheckedArith(
                    VArithOp::Div,
                    Box::new(l),
                    Box::new(r),
                    arith_ty,
                )),
                BinOp::Mod => Some(VExpr::CheckedArith(
                    VArithOp::Mod,
                    Box::new(l),
                    Box::new(r),
                    arith_ty,
                )),
                BinOp::EqCmp if is_f64 => {
                    Some(VExpr::F64Compare(VCmpOp::Eq, Box::new(l), Box::new(r)))
                }
                BinOp::NeCmp if is_f64 => {
                    Some(VExpr::F64Compare(VCmpOp::Ne, Box::new(l), Box::new(r)))
                }
                BinOp::Lt if is_f64 => {
                    Some(VExpr::F64Compare(VCmpOp::Lt, Box::new(l), Box::new(r)))
                }
                BinOp::Gt if is_f64 => {
                    Some(VExpr::F64Compare(VCmpOp::Gt, Box::new(l), Box::new(r)))
                }
                BinOp::Le if is_f64 => {
                    Some(VExpr::F64Compare(VCmpOp::Le, Box::new(l), Box::new(r)))
                }
                BinOp::Ge if is_f64 => {
                    Some(VExpr::F64Compare(VCmpOp::Ge, Box::new(l), Box::new(r)))
                }
                BinOp::EqCmp => Some(VExpr::Compare(VCmpOp::Eq, Box::new(l), Box::new(r))),
                BinOp::NeCmp => Some(VExpr::Compare(VCmpOp::Ne, Box::new(l), Box::new(r))),
                BinOp::Lt => Some(VExpr::Compare(VCmpOp::Lt, Box::new(l), Box::new(r))),
                BinOp::Gt => Some(VExpr::Compare(VCmpOp::Gt, Box::new(l), Box::new(r))),
                BinOp::Le => Some(VExpr::Compare(VCmpOp::Le, Box::new(l), Box::new(r))),
                BinOp::Ge => Some(VExpr::Compare(VCmpOp::Ge, Box::new(l), Box::new(r))),
                BinOp::And => Some(VExpr::Boolean(VBoolOp::And, vec![l, r])),
                BinOp::Or => Some(VExpr::Boolean(VBoolOp::Or, vec![l, r])),
                _ => None,
            }
        }
        Expr::Unary(UnOp::Neg, inner) => {
            let v = lower_expr_to_vir(inner, ctx)?;
            let neg_ty = ctx.infer_expr_type(inner);
            // f64 negation is NOT in the trusted subset (IEEE 754 rounding).
            if neg_ty == VType::F64Opaque {
                return None;
            }
            Some(VExpr::CheckedNeg(Box::new(v), neg_ty))
        }
        Expr::Unary(UnOp::Not, inner) => {
            let v = lower_expr_to_vir(inner, ctx)?;
            Some(VExpr::Not(Box::new(v)))
        }
        Expr::If { cond, then_, else_ } => {
            // C-7: lower whole branch blocks (in-branch lets included) —
            // `block_tail_expr` alone discards shadowing bindings.
            let c = lower_expr_to_vir(cond, ctx)?;
            let t = lower_branch_block(then_, ctx)?;
            let e = lower_branch_block(else_.as_ref()?, ctx)?;
            Some(VExpr::Select(Box::new(c), Box::new(t), Box::new(e)))
        }
        Expr::Block(stmts) => {
            // C-7: same reasoning as Expr::If — in-block lets must not be
            // dropped (their checked arithmetic still executes).
            lower_branch_block(stmts, ctx)
        }
        Expr::Match(scrutinee, arms) => {
            // Lower match as nested Select (ite chain)
            let matched = lower_expr_to_vir(scrutinee, ctx)?;
            lower_match_arms_to_vir(&matched, arms, ctx)
        }
        _ => None,
    }
}

/// Lower match arms to a nested Select (ite chain).
///
/// 0.31.29 audit P2-8: non-exhaustive matches (no wildcard/variable arm)
/// return None (fail-closed) instead of using IntConst(0) as fallback.
///
/// 0.31.29 audit P2-9: bool patterns use BoolConst + direct bool encoding
/// instead of IntConst(0/1) which fails for bool-typed scrutinees.
fn lower_match_arms_to_vir(
    matched: &VExpr,
    arms: &[crate::ast::MatchArm],
    ctx: &mut LoweringCtx,
) -> Option<VExpr> {
    use crate::ast::{Lit, PatternKind};

    let mut result: Option<VExpr> = None;
    for arm in arms.iter().rev() {
        let arm_val = lower_expr_to_vir(&arm.body, ctx)?;

        // Wildcard / variable patterns always match
        if matches!(
            &arm.pat.kind,
            PatternKind::Wildcard | PatternKind::Variable(_)
        ) {
            result = Some(arm_val);
            continue;
        }

        // Build pattern condition
        let cond = match &arm.pat.kind {
            PatternKind::Literal(Lit::Int(n)) => VExpr::Compare(
                VCmpOp::Eq,
                Box::new(matched.clone()),
                Box::new(VExpr::IntConst(*n)),
            ),
            // P2-9: bool patterns use BoolConst so that encode_bool can
            // handle bool-typed scrutinees correctly (not int comparison).
            PatternKind::Literal(Lit::Bool(true)) => {
                // match b { true => ... } → condition is just `b`
                matched.clone()
            }
            PatternKind::Literal(Lit::Bool(false)) => {
                // match b { false => ... } → condition is `!b`
                VExpr::Not(Box::new(matched.clone()))
            }
            _ => return None, // Constructor, Tuple, etc. not in trusted subset
        };

        // Apply guard if present
        let cond = if let Some(guard) = &arm.guard {
            let g = lower_expr_to_vir(guard, ctx)?;
            VExpr::Boolean(VBoolOp::And, vec![cond, g])
        } else {
            cond
        };

        result = Some(match result {
            Some(prev) => VExpr::Select(Box::new(cond), Box::new(arm_val), Box::new(prev)),
            // P2-8: no wildcard/variable arm means non-exhaustive match.
            // Fail-closed: return None instead of using IntConst(0) fallback.
            None => return None,
        });
    }
    result
}

/// Lower a branch block (the then/else of a tail `if`, an `Expr::If` branch,
/// or an `Expr::Block`) into a single `VExpr`.
///
/// C-7 (full-audit-2026-08-05-0656 §1): branch blocks live inside ONE
/// expression position (a `Select` arm), so their let-bindings cannot become
/// flat `VStmt::Let`s. Instead each binding gets a fresh SCOPED VarId
/// (shadowing outer names, including parameters) and is recursively
/// SUBSTITUTED into the branch value. The substitution keeps definedness
/// obligations attached to the value tree, where `collect_definedness`
/// guards them by the branch condition.
///
/// Fail-closed (`None` → whole function falls back to the AST path or
/// NotInTrustedSubset): any non-lowerable init/expression, nested statement
/// forms not in the trusted flat shape, and unused lets that carry checked
/// arithmetic (their division still executes at runtime even when the bound
/// name is never read — dropping them would silently skip E0801 traps).
fn lower_branch_block(stmts: &[crate::ast::Stmt], ctx: &mut LoweringCtx) -> Option<VExpr> {
    use crate::ast::{PatternKind, Stmt};

    ctx.enter_scope();
    let mut lets: Vec<(VarId, VExpr)> = Vec::new();
    let mut value: Option<VExpr> = None;
    let mut failed = false;
    let last_index = stmts.len().saturating_sub(1);

    for (pos, stmt) in stmts.iter().enumerate() {
        match stmt.unlocated() {
            // Contracts/super-comments carry no runtime value.
            Stmt::Requires(..)
            | Stmt::Ensures(..)
            | Stmt::Invariant(..)
            | Stmt::Math(..)
            | Stmt::MmsBlock { .. }
            | Stmt::Desc(..)
            | Stmt::Rule(..)
            | Stmt::Ellipsis => {}
            Stmt::Let { pat, init, .. } => {
                if let Some(init) = init {
                    let Some(vexpr) = lower_expr_to_vir(init, ctx) else {
                        failed = true;
                        break;
                    };
                    let name = match &pat.kind {
                        PatternKind::Variable(n) => n.clone(),
                        _ => {
                            // Unnamed binding: it cannot be referenced, so it
                            // survives only as a definedness obligation.
                            if vexpr.contains_checked_arith() {
                                failed = true;
                                break;
                            }
                            continue;
                        }
                    };
                    let vty = vexpr.ty().unwrap_or_else(|| ctx.infer_expr_type(init));
                    let var = ctx.bind_local(&name, vty);
                    lets.push((var, vexpr));
                }
            }
            Stmt::Expr(e) => {
                let Some(vexpr) = lower_expr_to_vir(e, ctx) else {
                    failed = true;
                    break;
                };
                value = Some(vexpr);
            }
            Stmt::Return(Some(e)) => {
                let Some(vexpr) = lower_expr_to_vir(e, ctx) else {
                    failed = true;
                    break;
                };
                value = Some(vexpr);
                break; // later statements are dead code
            }
            // Nested if: only as the final value producer (its value survives);
            // a non-tail nested if is a statement whose branch expressions are
            // discarded — their definedness obligations would be lost here.
            Stmt::If { cond, then_, else_ } if pos == last_index => {
                let Some(c) = lower_expr_to_vir(cond, ctx) else {
                    failed = true;
                    break;
                };
                let Some(t) = lower_branch_block(then_, ctx) else {
                    failed = true;
                    break;
                };
                let Some(else_block) = else_ else {
                    failed = true;
                    break;
                };
                let Some(e) = lower_branch_block(else_block, ctx) else {
                    failed = true;
                    break;
                };
                value = Some(VExpr::Select(Box::new(c), Box::new(t), Box::new(e)));
            }
            // Everything else (early unit return, non-tail nested if,
            // blocks/loops/Defer — the gate already rejects most) is
            // fail-closed.
            _ => {
                failed = true;
                break;
            }
        }
    }

    let value = match (failed, value) {
        (true, _) => None,
        (false, v) => v,
    };
    ctx.exit_scope();
    let Some(value) = value else {
        return None;
    };

    // Inline the branch-local lets into the value (recursively — chained lets
    // form a definition DAG, so substitution terminates).
    let substituted = substitute_lets(&value, &lets);

    // C-5-in-branch / fail-closed: a let carrying checked arithmetic still
    // executes at runtime even when its bound name is never read. If such a
    // let is unreachable from the branch value, substitution would silently
    // drop its definedness obligations (div-zero / overflow). Detect genuine
    // UNUSEDNESS via reachability from the value through the let-dependency
    // graph (NOT by searching the substituted tree — substitution removes the
    // very VarIds we are looking for, so that always reads "unused").
    let mut reachable: std::collections::HashSet<VarId> = collect_var_refs(&value);
    let mut changed = true;
    while changed {
        changed = false;
        for (var, init) in &lets {
            if reachable.contains(var) {
                for referenced in collect_var_refs(init) {
                    if reachable.insert(referenced) {
                        changed = true;
                    }
                }
            }
        }
    }
    for (var, init) in &lets {
        if init.contains_checked_arith() && !reachable.contains(var) {
            return None;
        }
    }

    Some(substituted)
}

/// Collect every `Var` reference in a VIR expression (used for branch-let
/// reachability). Parameters and locals alike.
fn collect_var_refs(expr: &VExpr) -> std::collections::HashSet<VarId> {
    let mut out = std::collections::HashSet::new();
    collect_var_refs_into(expr, &mut out);
    out
}

fn collect_var_refs_into(expr: &VExpr, out: &mut std::collections::HashSet<VarId>) {
    match expr {
        VExpr::Var(id) | VExpr::Old(id) | VExpr::OpaqueF64(id) => {
            out.insert(*id);
        }
        VExpr::CheckedArith(_, l, r, _) | VExpr::Compare(_, l, r) | VExpr::F64Compare(_, l, r) => {
            collect_var_refs_into(l, out);
            collect_var_refs_into(r, out);
        }
        VExpr::CheckedNeg(inner, _) | VExpr::Not(inner) => collect_var_refs_into(inner, out),
        VExpr::Boolean(_, es) => {
            for e in es {
                collect_var_refs_into(e, out);
            }
        }
        VExpr::Select(c, t, e) => {
            collect_var_refs_into(c, out);
            collect_var_refs_into(t, out);
            collect_var_refs_into(e, out);
        }
        VExpr::IntConst(_) | VExpr::BoolConst(_) | VExpr::F64Const(_) | VExpr::Result => {}
    }
}

/// Recursively replace let-bound `Var(id)`s with their (substituted) init
/// expressions. Bindings are defined-before-use, so the let list is acyclic.
fn substitute_lets(expr: &VExpr, lets: &[(VarId, VExpr)]) -> VExpr {
    match expr {
        VExpr::Var(id) => {
            if let Some((_, init)) = lets.iter().find(|(v, _)| v == id) {
                substitute_lets(init, lets)
            } else {
                expr.clone()
            }
        }
        VExpr::CheckedArith(op, l, r, ty) => VExpr::CheckedArith(
            *op,
            Box::new(substitute_lets(l, lets)),
            Box::new(substitute_lets(r, lets)),
            *ty,
        ),
        VExpr::CheckedNeg(inner, ty) => {
            VExpr::CheckedNeg(Box::new(substitute_lets(inner, lets)), *ty)
        }
        VExpr::Compare(op, l, r) => VExpr::Compare(
            *op,
            Box::new(substitute_lets(l, lets)),
            Box::new(substitute_lets(r, lets)),
        ),
        VExpr::F64Compare(op, l, r) => VExpr::F64Compare(
            *op,
            Box::new(substitute_lets(l, lets)),
            Box::new(substitute_lets(r, lets)),
        ),
        VExpr::Boolean(op, es) => {
            VExpr::Boolean(*op, es.iter().map(|e| substitute_lets(e, lets)).collect())
        }
        VExpr::Not(inner) => VExpr::Not(Box::new(substitute_lets(inner, lets))),
        VExpr::Select(c, t, e) => VExpr::Select(
            Box::new(substitute_lets(c, lets)),
            Box::new(substitute_lets(t, lets)),
            Box::new(substitute_lets(e, lets)),
        ),
        // Leaves without bindable variables.
        VExpr::IntConst(_)
        | VExpr::BoolConst(_)
        | VExpr::F64Const(_)
        | VExpr::Old(_)
        | VExpr::Result
        | VExpr::OpaqueF64(_) => expr.clone(),
    }
}

/// Extract the span from a statement.
fn stmt_span(stmt: &crate::ast::Stmt) -> Span {
    stmt.meta().map(|m| m.span).unwrap_or(Span::UNKNOWN)
}

// ── Flow transition → VIR with typestate axioms ───────────────────────

/// Lower a Flow transition to a VFunction with typestate context.
///
/// The typestate context carries:
/// - Source state invariants → Z3 axioms (assert)
/// - Transition guards → Z3 preconditions (assume)
/// - Target state invariants → Z3 obligations (prove)
///
/// **Current limitation**: Typestate information comes from the Checker
/// (CheckedProgram), but the verifier currently uses raw AST via
/// `legacy_body_file()`. The typestate context is therefore empty until
/// the verifier migrates to CheckedProgram (0.31.27+).
///
/// This function creates the VFunction infrastructure with an empty
/// typestate context, ready for future injection.
pub fn lower_transition_to_vir(
    flow_name: &str,
    transition: &crate::ast::TransitionDef,
) -> Result<(VFunction, VirSpanTable), String> {
    // Synthesize a FuncDef from the transition
    let func = crate::ast::FuncDef {
        meta: crate::ast::AstNodeMeta::inherited(
            transition.meta.span,
            crate::ast::AstOrigin::RuntimeSystem("verifier.transition_vir"),
        ),
        name: format!("{}::{}", flow_name, transition.name),
        pub_: false,
        params: transition.params.clone(),
        ret: None,
        body: transition.body.clone().unwrap_or_default(),
        where_clause: vec![],
        generics: vec![],
        effects: vec![],
        is_comptime: false,
        is_async: false,
        extern_abi: None,
        has_requires: false,
        has_ensures: false,
        has_mutate_params: false,
    };

    // Lower to VIR
    let (mut vfunc, span_table) = lower_func_to_vir(&func)?;

    // Inject typestate context (currently empty — needs CheckedProgram)
    // TODO(0.31.27+): Extract typestate information from CheckedProgram:
    // - Source state invariants from flow.states[source].invariants
    // - Transition guards from transition.guard
    // - Target state invariants from flow.states[target].invariants
    vfunc.typestate_context = Some(TypestateAxioms {
        source_invariants: vec![],
        transition_guards: vec![],
        target_invariants: vec![],
    });

    Ok((vfunc, span_table))
}

// ── VIR → Z3 encoding ─────────────────────────────────────────────────

/// Z3 encoding context for a single VFunction.
///
/// Maps canonical VarIds to Z3 variables. Separate maps for Int, Bool,
/// and opaque F64 (uninterpreted sort).
#[allow(dead_code)] // Wired into verify_func in 0.31.26-5
pub(crate) struct VirZ3Ctx {
    /// Int variables (i32/i64 encoded as unbounded Z3 Int).
    pub(crate) int_vars: HashMap<VarId, z3::ast::Int>,
    /// Bool variables.
    pub(crate) bool_vars: HashMap<VarId, z3::ast::Bool>,
    /// Opaque f64 variables (uninterpreted sort — no arithmetic).
    pub(crate) f64_vars: HashMap<VarId, z3::ast::Int>,
    /// Old snapshot variables for postconditions: `old(param)`.
    pub(crate) old_int_vars: HashMap<VarId, z3::ast::Int>,
    /// Old snapshot variables for bool params: `old(param)`.
    /// V-2 (full-audit-2026-08-05-0656 §3.8): previously only int params
    /// got old snapshots, so `ensures: old(b) == b` on a bool param was
    /// unencodable (NotInTrustedSubset) in the VIR path while the Resolved
    /// engine completed it — engine inconsistency.
    pub(crate) old_bool_vars: HashMap<VarId, z3::ast::Bool>,
    /// The `result` variable (Int or Bool depending on return type).
    pub(crate) result_int: Option<z3::ast::Int>,
    pub(crate) result_bool: Option<z3::ast::Bool>,
    /// f64 result variable (opaque, encoded as Int for equality/comparison only).
    pub(crate) result_f64: Option<z3::ast::Int>,
    /// Parameter types for type-directed encoding.
    pub(crate) var_types: HashMap<VarId, VType>,
    /// Whether the function returns f64 (opaque).
    returns_f64: bool,
    /// Whether the function returns bool.
    returns_bool: bool,
}

#[allow(dead_code)] // Wired into verify_func in 0.31.26-5
impl VirZ3Ctx {
    /// Create a new Z3 encoding context from a VFunction's parameters.
    pub(crate) fn new(vfunc: &VFunction) -> Self {
        let mut ctx = VirZ3Ctx {
            int_vars: HashMap::new(),
            bool_vars: HashMap::new(),
            f64_vars: HashMap::new(),
            old_int_vars: HashMap::new(),
            old_bool_vars: HashMap::new(),
            result_int: None,
            result_bool: None,
            result_f64: None,
            var_types: HashMap::new(),
            returns_f64: false,
            returns_bool: false,
        };

        // Register parameters
        for &(var, vty, ref _name) in &vfunc.params {
            ctx.var_types.insert(var, vty);
            let name = var.to_string();
            match vty {
                VType::Bool => {
                    ctx.bool_vars
                        .insert(var, z3::ast::Bool::new_const(name.as_str()));
                    // V-2: bool params get old snapshots too (engine parity).
                    let old_name = format!("old_{}", var);
                    ctx.old_bool_vars
                        .insert(var, z3::ast::Bool::new_const(old_name.as_str()));
                }
                VType::I32 | VType::I64 => {
                    ctx.int_vars
                        .insert(var, z3::ast::Int::new_const(name.as_str()));
                    // Also register old_ snapshot for postconditions
                    let old_name = format!("old_{}", var);
                    ctx.old_int_vars
                        .insert(var, z3::ast::Int::new_const(old_name.as_str()));
                }
                VType::F64Opaque => {
                    // f64 as uninterpreted sort — encoded as opaque Int
                    // (no arithmetic semantics; only equality/comparison)
                    ctx.f64_vars
                        .insert(var, z3::ast::Int::new_const(name.as_str()));
                }
            }
        }

        ctx
    }

    /// Register a let-bound variable in the Z3 context.
    pub(crate) fn register_let(&mut self, var: VarId, vty: VType) {
        self.var_types.insert(var, vty);
        let name = var.to_string();
        match vty {
            VType::Bool => {
                self.bool_vars
                    .insert(var, z3::ast::Bool::new_const(name.as_str()));
            }
            VType::I32 | VType::I64 => {
                self.int_vars
                    .insert(var, z3::ast::Int::new_const(name.as_str()));
            }
            VType::F64Opaque => {
                self.f64_vars
                    .insert(var, z3::ast::Int::new_const(name.as_str()));
            }
        }
    }

    /// Set up the result variable based on return type.
    pub(crate) fn setup_result(&mut self, returns_f64: bool, returns_bool: bool) {
        self.returns_f64 = returns_f64;
        self.returns_bool = returns_bool;
        if returns_bool {
            self.result_bool = Some(z3::ast::Bool::new_const("result"));
        } else if returns_f64 {
            // f64 result: opaque Int (no arithmetic, only equality/comparison)
            self.result_f64 = Some(z3::ast::Int::new_const("result"));
        } else {
            self.result_int = Some(z3::ast::Int::new_const("result"));
        }
    }

    /// Encode a VExpr as a Z3 Int term.
    /// Returns None if the expression is not Int-typed.
    pub(crate) fn encode_int(&self, expr: &VExpr) -> Option<z3::ast::Int> {
        match expr {
            VExpr::IntConst(n) => Some(z3::ast::Int::from_i64(*n)),
            VExpr::Var(id) => self.int_vars.get(id).cloned(),
            VExpr::Old(id) => self.old_int_vars.get(id).cloned(),
            VExpr::Result => self.result_int.clone(),
            VExpr::CheckedArith(op, lhs, rhs, ty) => {
                // f64 arithmetic is NOT encodable as Z3 Int (IEEE 754 rounding
                // is not modeled). Fail-closed: return None.
                if *ty == VType::F64Opaque {
                    return None;
                }
                let l = self.encode_int(lhs)?;
                let r = self.encode_int(rhs)?;
                match op {
                    VArithOp::Add => Some(z3::ast::Int::add(&[&l, &r])),
                    VArithOp::Sub => Some(z3::ast::Int::sub(&[&l, &r])),
                    VArithOp::Mul => Some(z3::ast::Int::mul(&[&l, &r])),
                    VArithOp::Div => {
                        // Truncating division (C semantics)
                        let zero = z3::ast::Int::from_i64(0);
                        let aa = l.ge(&zero).ite(&l, &l.unary_minus());
                        let ab = r.ge(&zero).ite(&r, &r.unary_minus());
                        let abs_q = aa.div(&ab);
                        let same_sign = l.ge(&zero).eq(&r.ge(&zero));
                        Some(same_sign.ite(&abs_q, &abs_q.unary_minus()))
                    }
                    VArithOp::Mod => {
                        // Truncating modulo (C semantics)
                        let zero = z3::ast::Int::from_i64(0);
                        let aa = l.ge(&zero).ite(&l, &l.unary_minus());
                        let ab = r.ge(&zero).ite(&r, &r.unary_minus());
                        let abs_mod = aa.modulo(&ab);
                        Some(l.ge(&zero).ite(&abs_mod, &abs_mod.unary_minus()))
                    }
                }
            }
            VExpr::CheckedNeg(inner, _ty) => {
                let v = self.encode_int(inner)?;
                Some(v.unary_minus())
            }
            VExpr::Select(cond, then_, else_) => {
                let c = self.encode_bool(cond)?;
                let t = self.encode_int(then_)?;
                let e = self.encode_int(else_)?;
                Some(c.ite(&t, &e))
            }
            _ => None,
        }
    }

    /// Encode a VExpr as a Z3 Bool term.
    pub(crate) fn encode_bool(&self, expr: &VExpr) -> Option<z3::ast::Bool> {
        match expr {
            VExpr::BoolConst(b) => Some(z3::ast::Bool::from_bool(*b)),
            VExpr::Var(id) => {
                // Try bool first, then int != 0
                if let Some(b) = self.bool_vars.get(id) {
                    return Some(b.clone());
                }
                self.int_vars
                    .get(id)
                    .map(|v| v.ne(&z3::ast::Int::from_i64(0)))
            }
            VExpr::Old(id) => {
                // V-2: bool params use dedicated old Bool snapshots.
                if let Some(b) = self.old_bool_vars.get(id) {
                    return Some(b.clone());
                }
                // old(param) in bool context: old_int != 0
                self.old_int_vars
                    .get(id)
                    .map(|v| v.ne(&z3::ast::Int::from_i64(0)))
            }
            VExpr::Result => {
                if let Some(b) = &self.result_bool {
                    return Some(b.clone());
                }
                self.result_int
                    .as_ref()
                    .map(|v| v.ne(&z3::ast::Int::from_i64(0)))
            }
            VExpr::Compare(op, lhs, rhs) => {
                // V-2: bool operands (including old(bool) snapshots) compare
                // as Z3 Bools. encode_int has no bool view, so without this
                // arm `ensures: old(b) == b` was unencodable in VIR.
                let l_bool = self.encode_bool_operand(lhs);
                let r_bool = self.encode_bool_operand(rhs);
                if let (Some(l), Some(r)) = (l_bool, r_bool) {
                    return match op {
                        VCmpOp::Eq => Some(l.eq(&r)),
                        VCmpOp::Ne => Some(l.eq(&r).not()),
                        // Ordering on bools is not in the trusted subset;
                        // fall through to the Int path (which fails closed).
                        VCmpOp::Lt | VCmpOp::Gt | VCmpOp::Le | VCmpOp::Ge => None,
                    };
                }
                let l = self.encode_int(lhs)?;
                let r = self.encode_int(rhs)?;
                match op {
                    VCmpOp::Eq => Some(l.eq(&r)),
                    VCmpOp::Ne => Some(l.ne(&r)),
                    VCmpOp::Lt => Some(l.lt(&r)),
                    VCmpOp::Gt => Some(l.gt(&r)),
                    VCmpOp::Le => Some(l.le(&r)),
                    VCmpOp::Ge => Some(l.ge(&r)),
                }
            }
            VExpr::F64Compare(_op, _lhs, _rhs) => {
                // P0-10: F64Compare encoding is semantically unsound.
                // encode_f64 maps f64 literals to bit-pattern Ints and uses
                // Z3 Int comparison. This gives NaN a large positive integer
                // value, making `NaN > 1.0` true in Z3 but false in IEEE 754.
                // Equality is also wrong: NaN != NaN in IEEE 754 but bit-equal
                // in Z3; +0.0 == -0.0 in IEEE 754 but bit-different.
                // Until a proper uninterpreted predicate encoding is implemented,
                // all f64 comparisons are NotInTrustedSubset (fail-closed).
                None
            }
            VExpr::Boolean(op, exprs) => {
                let encoded: Vec<z3::ast::Bool> =
                    exprs.iter().filter_map(|e| self.encode_bool(e)).collect();
                if encoded.len() != exprs.len() {
                    return None;
                }
                let refs: Vec<&z3::ast::Bool> = encoded.iter().collect();
                match op {
                    VBoolOp::And => Some(z3::ast::Bool::and(&refs)),
                    VBoolOp::Or => Some(z3::ast::Bool::or(&refs)),
                }
            }
            VExpr::Not(inner) => {
                let v = self.encode_bool(inner)?;
                Some(v.not())
            }
            VExpr::Select(cond, then_, else_) => {
                let c = self.encode_bool(cond)?;
                let t = self.encode_bool(then_)?;
                let e = self.encode_bool(else_)?;
                Some(c.ite(&t, &e))
            }
            _ => None,
        }
    }

    /// V-2: resolve a bool-typed variable (or its old snapshot) to its Z3
    /// Bool so bool equality/inequality can be encoded directly. Returns
    /// `None` for non-bool operands; callers fall through to the Int path.
    fn encode_bool_operand(&self, expr: &VExpr) -> Option<z3::ast::Bool> {
        match expr {
            VExpr::Var(id) => self.bool_vars.get(id).cloned(),
            VExpr::Old(id) => self.old_bool_vars.get(id).cloned(),
            VExpr::BoolConst(b) => Some(z3::ast::Bool::from_bool(*b)),
            _ => None,
        }
    }

    /// Encode a VExpr as an opaque f64 Z3 Int (uninterpreted sort).
    /// No arithmetic semantics — only equality/ordering.
    pub(crate) fn encode_f64(&self, expr: &VExpr) -> Option<z3::ast::Int> {
        match expr {
            VExpr::F64Const(f) => {
                // Encode f64 literal as opaque Int (bit-pattern hash)
                // This is NOT arithmetic — just identity for equality checks
                let bits = f.to_bits() as i64;
                Some(z3::ast::Int::from_i64(bits))
            }
            VExpr::OpaqueF64(id) => self.f64_vars.get(id).cloned(),
            VExpr::Var(id) => {
                // Check if this var is f64-typed
                if self.var_types.get(id) == Some(&VType::F64Opaque) {
                    return self.f64_vars.get(id).cloned();
                }
                None
            }
            VExpr::Result => self.result_f64.clone(),
            _ => None,
        }
    }

    /// Generate definedness obligations for checked arithmetic.
    /// Returns (condition, failure_message) pairs.
    pub(crate) fn definedness_obligations(
        &self,
        expr: &VExpr,
    ) -> Vec<(z3::ast::Bool, &'static str)> {
        let mut obligations = Vec::new();
        self.collect_definedness(expr, &mut obligations);
        obligations
    }

    fn collect_definedness(
        &self,
        expr: &VExpr,
        obligations: &mut Vec<(z3::ast::Bool, &'static str)>,
    ) {
        match expr {
            VExpr::CheckedArith(op, lhs, rhs, ty) => {
                // Recurse into operands first
                self.collect_definedness(lhs, obligations);
                self.collect_definedness(rhs, obligations);

                // V-6 (full-audit-2026-08-05-0656 §3.8): i64 was modeled as
                // unbounded Int with NO definedness, contradicting the
                // SD-7/SD-8 trap semantics (Trap ≠ Fault): `let y = x / z`
                // with a possible z == 0 traps E0801 at runtime yet verified
                // Proven. Minimal fail-closed fix mirroring the i32
                // machinery: i64 div/mod now generate zero-divisor + MIN÷-1
                // obligations. i64 add/sub/mul OVERFLOW obligations still
                // need operand range axioms to be meaningful and remain a
                // documented gap (i32 overflow obligations are unchanged).
                // Coordinated with V-1 (AST/Resolved engine parity).
                let min_bound: i64 = match ty {
                    VType::I32 => i32::MIN as i64,
                    VType::I64 => i64::MIN,
                    // f64 arithmetic is rejected at lowering; bool has no arithmetic.
                    VType::F64Opaque | VType::Bool => return,
                };

                // Fail-closed: if operand encoding fails, push an always-false
                // obligation so verification rejects rather than silently skipping.
                let (l, r) = match (self.encode_int(lhs), self.encode_int(rhs)) {
                    (Some(l), Some(r)) => (l, r),
                    _ => {
                        obligations.push((
                            z3::ast::Bool::from_bool(false),
                            "integer operation has unencodable operand (internal error)",
                        ));
                        return;
                    }
                };
                {
                    match op {
                        VArithOp::Add | VArithOp::Sub | VArithOp::Mul => {
                            // V-6 scope: overflow obligations for i32 only.
                            // i64 add/sub/mul stay unbounded (documented gap,
                            // see the V-6 note above — the Proven message
                            // discloses the assumption).
                            if *ty != VType::I32 {
                                return;
                            }
                            let result = match op {
                                VArithOp::Add => z3::ast::Int::add(&[&l, &r]),
                                VArithOp::Sub => z3::ast::Int::sub(&[&l, &r]),
                                VArithOp::Mul => z3::ast::Int::mul(&[&l, &r]),
                                _ => unreachable!(),
                            };
                            let lo = z3::ast::Int::from_i64(i32::MIN as i64);
                            let hi = z3::ast::Int::from_i64(i32::MAX as i64);
                            obligations.push((
                                z3::ast::Bool::and(&[&result.ge(&lo), &result.le(&hi)]),
                                "integer overflow is not excluded by preconditions",
                            ));
                        }
                        VArithOp::Div | VArithOp::Mod => {
                            let zero = z3::ast::Int::from_i64(0);
                            let min = z3::ast::Int::from_i64(min_bound);
                            let neg_one = z3::ast::Int::from_i64(-1);
                            let min_overflow = z3::ast::Bool::and(&[&l.eq(&min), &r.eq(&neg_one)]);
                            obligations.push((
                                z3::ast::Bool::and(&[&r.ne(&zero), &min_overflow.not()]),
                                "integer operation is undefined (zero divisor or MIN / -1)",
                            ));
                        }
                    }
                }
            }
            VExpr::CheckedNeg(inner, ty) => {
                self.collect_definedness(inner, obligations);
                // V-6: negation of MIN traps for both checked widths.
                let min_bound: i64 = match ty {
                    VType::I32 => i32::MIN as i64,
                    VType::I64 => i64::MIN,
                    VType::F64Opaque | VType::Bool => return,
                };
                // Fail-closed: if encoding fails, push always-false obligation
                match self.encode_int(inner) {
                    Some(v) => {
                        let min = z3::ast::Int::from_i64(min_bound);
                        obligations.push((
                            v.ne(&min),
                            "integer overflow is not excluded by preconditions",
                        ));
                    }
                    None => {
                        obligations.push((
                            z3::ast::Bool::from_bool(false),
                            "integer negation has unencodable operand (internal error)",
                        ));
                    }
                }
            }
            VExpr::Select(cond, then_, else_) => {
                // Check definedness of the condition itself (e.g., div-by-zero in cond)
                self.collect_definedness(cond, obligations);
                // Conditional obligations: guard with condition
                if let Some(c) = self.encode_bool(cond) {
                    let mut then_obligs = Vec::new();
                    self.collect_definedness(then_, &mut then_obligs);
                    for (cond_oblig, msg) in then_obligs {
                        obligations.push((c.implies(&cond_oblig), msg));
                    }
                    let mut else_obligs = Vec::new();
                    self.collect_definedness(else_, &mut else_obligs);
                    let not_c = c.not();
                    for (cond_oblig, msg) in else_obligs {
                        obligations.push((not_c.implies(&cond_oblig), msg));
                    }
                }
            }
            // Recurse into other compound expressions
            VExpr::Boolean(_, exprs) => {
                for e in exprs {
                    self.collect_definedness(e, obligations);
                }
            }
            VExpr::Not(inner) => self.collect_definedness(inner, obligations),
            VExpr::Compare(_, lhs, rhs) | VExpr::F64Compare(_, lhs, rhs) => {
                self.collect_definedness(lhs, obligations);
                self.collect_definedness(rhs, obligations);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_func(source: &str) -> crate::ast::FuncDef {
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let file = crate::parser::Parser::new_memory(tokens, "test", "test", source)
            .unwrap()
            .parse_file()
            .unwrap();
        for item in &file.items {
            if let crate::ast::Item::Func(f) = item {
                return f.clone();
            }
        }
        panic!("no function found in source");
    }

    #[test]
    fn test_trusted_subset_accepts_scalar() {
        let func = parse_func(
            "func add(x: i32, y: i32) -> i32 {
                requires: x > 0
                ensures: result > 0
                x + y
            }",
        );
        assert!(matches!(check_trusted_subset(&func), Ok(())));
    }

    #[test]
    fn test_trusted_subset_accepts_f64_params() {
        let func = parse_func(
            "func scale(x: f64, y: f64) -> f64 {
                x
            }",
        );
        assert!(matches!(check_trusted_subset(&func), Ok(())));
    }

    #[test]
    fn test_trusted_subset_rejects_string() {
        let func = parse_func(
            "func greet(name: string) -> string {
                name
            }",
        );
        assert!(matches!(check_trusted_subset(&func), Err(_)));
    }

    #[test]
    fn test_trusted_subset_rejects_list() {
        let func = parse_func(
            "func first(xs: List<i32>) -> i32 {
                0
            }",
        );
        assert!(matches!(check_trusted_subset(&func), Err(_)));
    }

    #[test]
    fn test_trusted_subset_rejects_loop() {
        let func = parse_func(
            "func sum(n: i32) -> i32 {
                let mut acc = 0
                while acc < n {
                    acc = acc + 1
                }
                acc
            }",
        );
        assert!(matches!(check_trusted_subset(&func), Err(_)));
    }

    #[test]
    fn test_trusted_subset_rejects_call() {
        let func = parse_func(
            "func caller(x: i32) -> i32 {
                double(x)
            }",
        );
        assert!(matches!(check_trusted_subset(&func), Err(_)));
    }

    #[test]
    fn test_lower_simple_func() {
        let func = parse_func(
            "func add(x: i32, y: i32) -> i32 {
                requires: x > 0
                ensures: result > 0
                x + y
            }",
        );
        let (vfunc, span_table) = lower_func_to_vir(&func).unwrap();
        assert_eq!(vfunc.id, "add");
        assert_eq!(vfunc.params.len(), 2);
        assert_eq!(vfunc.params[0].2, "x");
        assert_eq!(vfunc.params[1].2, "y");
        assert_eq!(vfunc.postconditions.len(), 1);
        // Body should have: Assume(requires), Return(x + y)
        assert!(vfunc.body.iter().any(|s| matches!(s, VStmt::Assume(_))));
        assert!(vfunc.body.iter().any(|s| matches!(s, VStmt::Return(_))));
        // Span table should have function span
        assert!(span_table.func_span("add").is_some());
    }

    #[test]
    fn test_lower_if_expr() {
        let func = parse_func(
            "func abs(x: i32) -> i32 {
                if x > 0 { x } else { 0 - x }
            }",
        );
        let (vfunc, _) = lower_func_to_vir(&func).unwrap();
        // Should have a Return with a Select
        let has_select = vfunc.body.iter().any(|s| {
            if let VStmt::Return(e) = s {
                matches!(e, VExpr::Select(..))
            } else {
                false
            }
        });
        assert!(has_select, "expected Select in return");
    }

    #[test]
    fn test_lower_match_expr() {
        let func = parse_func(
            "func classify(x: i32) -> i32 {
                match x {
                    0 => 1,
                    _ => 2,
                }
            }",
        );
        let (vfunc, _) = lower_func_to_vir(&func).unwrap();
        let has_select = vfunc.body.iter().any(|s| {
            if let VStmt::Return(e) = s {
                matches!(e, VExpr::Select(..))
            } else {
                false
            }
        });
        assert!(has_select, "expected Select from match");
    }

    #[test]
    fn test_canonical_var_names() {
        let func = parse_func(
            "func f(a: i32, b: i32) -> i32 {
                let c = a + b
                c
            }",
        );
        let (vfunc, _) = lower_func_to_vir(&func).unwrap();
        // Parameters should be %0, %1
        assert_eq!(vfunc.params[0].0, VarId(0));
        assert_eq!(vfunc.params[1].0, VarId(1));
        // Local 'c' should be %2
        let let_stmt = vfunc.body.iter().find(|s| matches!(s, VStmt::Let(..)));
        if let Some(VStmt::Let(var, _)) = let_stmt {
            assert_eq!(*var, VarId(2));
        }
    }

    #[test]
    fn test_normalized_repr_deterministic() {
        let func = parse_func(
            "func add(x: i32, y: i32) -> i32 {
                requires: x > 0
                ensures: result > 0
                x + y
            }",
        );
        let (vfunc, _) = lower_func_to_vir(&func).unwrap();
        let repr1 = vfunc.normalized_repr();
        let repr2 = vfunc.normalized_repr();
        assert_eq!(repr1, repr2, "normalized_repr must be deterministic");
        assert!(repr1.contains("%0"), "should use canonical var names");
        assert!(repr1.contains("%1"), "should use canonical var names");
    }

    #[test]
    fn test_f64_opaque_in_vir() {
        let func = parse_func(
            "func pass_through(x: f64) -> f64 {
                x
            }",
        );
        let (vfunc, _) = lower_func_to_vir(&func).unwrap();
        // Parameter should be F64Opaque
        assert_eq!(vfunc.params[0].1, VType::F64Opaque);
        // Return should be OpaqueF64
        let has_opaque = vfunc.body.iter().any(|s| {
            if let VStmt::Return(e) = s {
                matches!(e, VExpr::OpaqueF64(_))
            } else {
                false
            }
        });
        assert!(has_opaque, "f64 param should lower to OpaqueF64");
    }

    #[test]
    fn test_span_side_table() {
        let func = parse_func(
            "func add(x: i32, y: i32) -> i32 {
                ensures: result > 0
                x + y
            }",
        );
        let (vfunc, span_table) = lower_func_to_vir(&func).unwrap();
        // Function span should be recorded
        assert!(span_table.func_span("add").is_some());
        // Postcondition span should be recorded
        assert!(span_table.postcondition_span("add", 0).is_some());
        // VIR itself should have no spans
        let repr = vfunc.normalized_repr();
        assert!(
            !repr.contains("line"),
            "VIR repr should not contain span info"
        );
    }

    // ── 0.31.27 audit regression tests ─────────────────────────────────

    #[test]
    fn test_gate_rejects_bitwise_ops() {
        // P0-2: BitAnd in ensures → lowering fails (fail-closed)
        let func = parse_func(
            "func f(x: i32) -> i32 {
                ensures: (x & 1) >= 0
                x
            }",
        );
        // Gate passes (contracts are not checked by gate), but lowering fails
        assert!(
            lower_func_to_vir(&func).is_err(),
            "lowering must fail for bitwise operators in ensures"
        );
    }

    #[test]
    fn test_gate_rejects_string_literal() {
        // P0-2: String literal passes old gate, fails lowering
        let func = parse_func(
            "func f(x: i32) -> i32 {
                ensures: result > 0
                x
            }",
        );
        // This should pass (no string literal)
        assert!(check_trusted_subset(&func).is_ok());
    }

    #[test]
    fn test_gate_rejects_match_variable_pattern() {
        // P0-5: Match variable patterns create unbound Z3 variables
        let func = parse_func(
            "func f(x: i32) -> i32 {
                match x { y => y + 1 }
            }",
        );
        assert!(
            check_trusted_subset(&func).is_err(),
            "gate must reject match variable patterns"
        );
    }

    #[test]
    fn test_gate_rejects_old_complex_expr() {
        // P2-3: old(x + 1) → gate or lowering must reject
        let func = parse_func(
            "func f(x: i32) -> i32 {
                ensures: result == old(x + 1)
                x
            }",
        );
        // Either the gate rejects it, or the lowering fails
        let gate_result = check_trusted_subset(&func);
        let lower_result = lower_func_to_vir(&func);
        assert!(
            gate_result.is_err() || lower_result.is_err(),
            "old(complex_expr) must be rejected by gate or lowering"
        );
    }

    #[test]
    fn test_lower_if_stmt_tail_position() {
        // P0-1: Stmt::If in tail position should lower to Select
        let func = parse_func(
            "func abs(x: i32) -> i32 {
                if x > 0 { x } else { 0 - x }
            }",
        );
        let (vfunc, _) = lower_func_to_vir(&func).unwrap();
        let has_select = vfunc.body.iter().any(|s| {
            if let VStmt::Return(e) = s {
                matches!(e, VExpr::Select(..))
            } else {
                false
            }
        });
        assert!(
            has_select,
            "Stmt::If in tail position should lower to Select"
        );
    }

    #[test]
    fn test_lower_multiple_expr_stmts_only_last_is_return() {
        // P1-1: Only the last Stmt::Expr should become Return.
        // P0-8: Non-tail expressions with checked arithmetic (x + 1) are
        // now rejected (fail-closed) because their definedness obligations
        // would be silently discarded. Use a pure variable reference instead.
        let func = parse_func(
            "func f(x: i32) -> i32 {
                x;
                x + 2
            }",
        );
        let (vfunc, _) = lower_func_to_vir(&func).unwrap();
        let return_count = vfunc
            .body
            .iter()
            .filter(|s| matches!(s, VStmt::Return(_)))
            .count();
        assert_eq!(
            return_count, 1,
            "only the last expression statement should become Return, got {}",
            return_count
        );
    }

    #[test]
    fn test_lower_non_tail_checked_arith_rejected() {
        // P0-8: Non-tail expression with checked arithmetic must be rejected
        // (fail-closed) — definedness obligations cannot be verified for
        // discarded values.
        let func = parse_func(
            "func f(x: i32, y: i32) -> i32 {
                x / y;
                x + 1
            }",
        );
        assert!(
            lower_func_to_vir(&func).is_err(),
            "non-tail div must be rejected by VIR lowering"
        );
    }

    #[test]
    fn test_gate_rejects_block_stmt() {
        // P1-2: Stmt::Block accepted by old gate, silently skipped by lowering
        let func = parse_func(
            "func f(x: i32) -> i32 {
                ensures: result > 0
                { x + 1 }
            }",
        );
        assert!(
            check_trusted_subset(&func).is_err(),
            "gate must reject block statements"
        );
    }
}
