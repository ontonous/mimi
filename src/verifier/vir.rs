//! Verification IR (VIR) — span-free intermediate representation for the
//! Z3 verifier.
//!
//! Design constraints (verified-core-1.md §3):
//! - VIR nodes carry NO Span; spans live in a side table for error reporting.
//! - Local variables use canonical names (`%0`, `%1`, `%2`, …) so that
//!   cosmetic renames do not invalidate the semantic hash.
//! - Only trusted-subset types are representable: bool, checked i32/i64,
//!   f64 as an opaque uninterpreted sort.
//! - `typestate_context` carries Flow transition axioms (source invariants,
//!   transition guards, target invariants).
//!
//! Lowering path: `FuncDef` (raw AST) → trusted-subset gate → `VFunction`.
//! The gate rejects unsupported constructs *before* any SMT encoding.

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
    pub fn normalized_repr(&self) -> String {
        let mut s = String::new();
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
        Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Invariant(..) | Stmt::Math(..) => {
            Ok(())
        }
        // Super-comments are ignored
        Stmt::MmsBlock { .. } | Stmt::Desc(..) | Stmt::Rule(..) | Stmt::Ellipsis => {
            Ok(())
        }
        // Let bindings: check the init expression
        Stmt::Let { init, .. } => {
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
        // If: check condition and branches
        Stmt::If {
            cond,
            then_,
            else_,
        } => {
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
        Stmt::Assign { .. } => Err(
            "mutation is not in the trusted subset (v1: immutable scalars only)".into(),
        ),
        // Defer is NOT in the trusted subset
        Stmt::Defer(..) => Err(
            "defer is not in the trusted subset".into(),
        ),
        // Block: recurse
        Stmt::Block(stmts) => check_stmts_trusted(stmts),
        // Anything else is rejected
        _ => Err(
            "statement is not in the trusted subset".into(),
        ),
    }
}

/// Recursively check expressions for trusted-subset compliance.
fn check_expr_trusted(expr: &crate::ast::Expr) -> TrustedSubsetResult {
    use crate::ast::Expr;
    match expr.unlocated() {
        // Literals are always trusted
        Expr::Literal(_) => Ok(()),
        // Identifiers are trusted (resolved by context)
        Expr::Ident(_) => Ok(()),
        // old(param) is trusted for immutable scalar parameters
        Expr::Old(inner) => check_expr_trusted(inner),
        // Binary operations: check operands
        Expr::Binary(_, lhs, rhs) => {
            check_expr_trusted(lhs)?;
            check_expr_trusted(rhs)
        }
        // Unary operations: check operand
        Expr::Unary(_, inner) => check_expr_trusted(inner),
        // If expression: check all branches
        Expr::If { cond, then_, else_ } => {
            check_expr_trusted(cond)?;
            if let Some(tail) = crate::verifier::helpers::block_tail_expr(then_) {
                check_expr_trusted(&tail)?;
            }
            if let Some(else_) = else_ {
                if let Some(tail) = crate::verifier::helpers::block_tail_expr(else_) {
                    check_expr_trusted(&tail)?;
                }
            }
            Ok(())
        }
        // Block: check tail expression
        Expr::Block(stmts) => {
            check_stmts_trusted(stmts)?;
            Ok(())
        }
        // Match: check scrutinee and arms
        Expr::Match(scrutinee, arms) => {
            check_expr_trusted(scrutinee)?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    check_expr_trusted(guard)?;
                }
                check_expr_trusted(&arm.body)?;
            }
            Ok(())
        }
        // Calls: NOT in trusted subset v1 (future: only Proven pure total acyclic)
        Expr::Call(callee, _) => {
            // Allow old() — it's handled by Expr::Old above
            if let Expr::Ident(name) = callee.unlocated() {
                if name == "old" {
                    return Ok(());
                }
            }
            Err(
                "function calls are not in the trusted subset (v1: no calls)".into(),
            )
        }
        // Field access: NOT in trusted subset (heap/aggregate)
        Expr::Field(..) => Err(
            "field access is not in the trusted subset (v1: no heap/aggregate)".into(),
        ),
        // Tuple index: NOT in trusted subset
        Expr::TupleIndex(..) => Err(
            "tuple index is not in the trusted subset (v1: no aggregates)".into(),
        ),
        // Spawn/Await: NOT in trusted subset
        Expr::Spawn(..) => Err(
            "spawn is not in the trusted subset".into(),
        ),
        Expr::Await(..) => Err(
            "await is not in the trusted subset".into(),
        ),
        // Anything else is rejected
        _ => Err(
            "expression is not in the trusted subset".into(),
        ),
    }
}

// ── Lowering: FuncDef → VFunction ──────────────────────────────────────

/// Lowering context: maps source names to canonical VarIds.
#[allow(dead_code)] // returns_f64/returns_bool/fresh_local: infrastructure for f64 opaque sort + local let lowering
struct LoweringCtx {
    /// Next canonical variable index.
    next_var: usize,
    /// Source name → canonical VarId.
    name_map: HashMap<String, VarId>,
    /// Parameter types for type resolution.
    param_types: HashMap<String, VType>,
    /// Whether the return type is f64.
    returns_f64: bool,
    /// Whether the return type is bool.
    returns_bool: bool,
}

impl LoweringCtx {
    fn new(func: &crate::ast::FuncDef) -> Self {
        let mut ctx = LoweringCtx {
            next_var: 0,
            name_map: HashMap::new(),
            param_types: HashMap::new(),
            returns_f64: func
                .ret
                .as_ref()
                .is_some_and(|t| matches!(t.unlocated(), crate::ast::Type::Name(n, _) if n == "f64")),
            returns_bool: func.ret.as_ref().is_some_and(
                |t| matches!(t.unlocated(), crate::ast::Type::Name(n, _) if n == "bool" || n == "Bool"),
            ),
        };
        // Register parameters as %0, %1, %2, ...
        for param in &func.params {
            let var = VarId(ctx.next_var);
            ctx.next_var += 1;
            ctx.name_map.insert(param.name.clone(), var);
            let vty = surface_type_to_vtype(&param.ty);
            ctx.param_types.insert(param.name.clone(), vty);
        }
        ctx
    }

    /// Get or create a canonical VarId for a source name.
    fn resolve(&mut self, name: &str) -> VarId {
        if let Some(&var) = self.name_map.get(name) {
            var
        } else {
            let var = VarId(self.next_var);
            self.next_var += 1;
            self.name_map.insert(name.to_string(), var);
            var
        }
    }

    /// Get the VType for a source name (parameter or local).
    fn type_of(&self, name: &str) -> Option<VType> {
        self.param_types.get(name).copied()
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
/// Panics if the type is not in the trusted subset (caller must check first).
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
pub fn lower_func_to_vir(
    func: &crate::ast::FuncDef,
) -> Result<(VFunction, VirSpanTable), String> {
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
            let var = ctx.name_map[&p.name];
            let vty = surface_type_to_vtype(&p.ty);
            (var, vty, p.name.clone())
        })
        .collect();

    // Extract contracts and body
    let mut body_stmts: Vec<VStmt> = Vec::new();
    let mut postconditions: Vec<VExpr> = Vec::new();
    let mut stmt_index = 0usize;

    for stmt in &func.body {
        match stmt.unlocated() {
            crate::ast::Stmt::Requires(expr, _) => {
                if let Some(vexpr) = lower_expr_to_vir(expr, &mut ctx) {
                    body_stmts.push(VStmt::Assume(vexpr));
                    span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                    stmt_index += 1;
                }
            }
            crate::ast::Stmt::Ensures(expr, _) => {
                if let Some(vexpr) = lower_expr_to_vir(expr, &mut ctx) {
                    let idx = postconditions.len();
                    postconditions.push(vexpr);
                    span_table.record_postcondition(&func_id, idx, stmt_span(stmt));
                }
            }
            crate::ast::Stmt::Invariant(expr, _) => {
                // Invariants are assumed (established from requires)
                if let Some(vexpr) = lower_expr_to_vir(expr, &mut ctx) {
                    body_stmts.push(VStmt::Assume(vexpr));
                    span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                    stmt_index += 1;
                }
            }
            crate::ast::Stmt::Math(exprs) => {
                for expr in exprs {
                    if let Some(vexpr) = lower_expr_to_vir(expr, &mut ctx) {
                        body_stmts.push(VStmt::Assert(vexpr));
                        span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                        stmt_index += 1;
                    }
                }
            }
            crate::ast::Stmt::Let { pat, init, .. } => {
                if let Some(init) = init {
                    if let Some(vexpr) = lower_expr_to_vir(init, &mut ctx) {
                        // Extract variable name from pattern
                        let name = match &pat.kind {
                            crate::ast::PatternKind::Variable(n) => n.clone(),
                            _ => format!("_let{}", stmt_index),
                        };
                        let var = ctx.resolve(&name);
                        body_stmts.push(VStmt::Let(var, vexpr));
                        span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                        stmt_index += 1;
                    }
                }
            }
            crate::ast::Stmt::Return(expr) => {
                if let Some(expr) = expr {
                    if let Some(vexpr) = lower_expr_to_vir(expr, &mut ctx) {
                        body_stmts.push(VStmt::Return(vexpr));
                        span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                        stmt_index += 1;
                    }
                }
            }
            crate::ast::Stmt::Expr(expr) => {
                // Tail expression → implicit return
                if let Some(vexpr) = lower_expr_to_vir(expr, &mut ctx) {
                    body_stmts.push(VStmt::Return(vexpr));
                    span_table.record_stmt(&func_id, stmt_index, stmt_span(stmt));
                    stmt_index += 1;
                }
            }
            // Skip super-comments and other non-semantic statements
            _ => {}
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
            match op {
                BinOp::Add => Some(VExpr::CheckedArith(
                    VArithOp::Add,
                    Box::new(l),
                    Box::new(r),
                    VType::I64,
                )),
                BinOp::Sub => Some(VExpr::CheckedArith(
                    VArithOp::Sub,
                    Box::new(l),
                    Box::new(r),
                    VType::I64,
                )),
                BinOp::Mul => Some(VExpr::CheckedArith(
                    VArithOp::Mul,
                    Box::new(l),
                    Box::new(r),
                    VType::I64,
                )),
                BinOp::Div => Some(VExpr::CheckedArith(
                    VArithOp::Div,
                    Box::new(l),
                    Box::new(r),
                    VType::I64,
                )),
                BinOp::Mod => Some(VExpr::CheckedArith(
                    VArithOp::Mod,
                    Box::new(l),
                    Box::new(r),
                    VType::I64,
                )),
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
            Some(VExpr::CheckedNeg(Box::new(v), VType::I64))
        }
        Expr::Unary(UnOp::Not, inner) => {
            let v = lower_expr_to_vir(inner, ctx)?;
            Some(VExpr::Not(Box::new(v)))
        }
        Expr::If { cond, then_, else_ } => {
            let c = lower_expr_to_vir(cond, ctx)?;
            let t = crate::verifier::helpers::block_tail_expr(then_)
                .and_then(|e| lower_expr_to_vir(&e, ctx))?;
            let e = else_
                .as_ref()
                .and_then(|b| crate::verifier::helpers::block_tail_expr(b))
                .and_then(|e| lower_expr_to_vir(&e, ctx))?;
            Some(VExpr::Select(Box::new(c), Box::new(t), Box::new(e)))
        }
        Expr::Block(stmts) => {
            let tail = crate::verifier::helpers::block_tail_expr(stmts)?;
            lower_expr_to_vir(&tail, ctx)
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
            PatternKind::Literal(Lit::Bool(b)) => VExpr::Compare(
                VCmpOp::Eq,
                Box::new(matched.clone()),
                Box::new(VExpr::IntConst(if *b { 1 } else { 0 })),
            ),
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
            None => VExpr::Select(
                Box::new(cond),
                Box::new(arm_val),
                Box::new(VExpr::IntConst(0)), // fallback for non-exhaustive
            ),
        });
    }
    result
}

/// Extract the span from a statement.
fn stmt_span(stmt: &crate::ast::Stmt) -> Span {
    stmt.meta()
        .map(|m| m.span)
        .unwrap_or(Span::UNKNOWN)
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
        assert!(!repr.contains("line"), "VIR repr should not contain span info");
    }
}
