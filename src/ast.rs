use crate::span::{SourceRegistry, Span};

#[derive(Debug, Clone)]
pub struct File {
    /// Source table for every source-aware span reachable from this file.
    /// IDs are dense session-local indexes; stable identity lives in SourceKey.
    pub sources: SourceRegistry,
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
    /// v0.29.22: true when this file was compiled in progressive Typestate
    /// "script mode" — no user `flow`/`state`/`transition`, so the compiler
    /// injected an implicit `flow Main { state Single }`. Mailbox closed,
    /// no external events; execution remains via top-level `main`.
    pub implicit_single: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstOrigin {
    User,
    Desugared(&'static str),
    PrototypeFallback(&'static str),
    RuntimeSystem(&'static str),
}

impl AstOrigin {
    /// Stable provenance category used by diagnostics and tooling.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Desugared(_) => "desugared",
            Self::PrototypeFallback(_) => "prototype_fallback",
            Self::RuntimeSystem(_) => "runtime_system",
        }
    }

    /// Lowering rule that created this node. User-written nodes have no rule.
    pub const fn rule(self) -> Option<&'static str> {
        match self {
            Self::User => None,
            Self::Desugared(rule) | Self::PrototypeFallback(rule) | Self::RuntimeSystem(rule) => {
                Some(rule)
            }
        }
    }
}

/// Structural parent requested by a generated AST node.
///
/// This deliberately does not depend on `core::NodeId`: the resolved walker
/// turns the hint into its canonical ID after it knows the enclosing semantic
/// owner and module path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AstParentHint {
    /// User nodes have no generated-origin parent. A generated node carrying
    /// this value is malformed and must be rejected by resolved lowering.
    #[default]
    None,
    /// The canonical structural owner selected by the AST walker.
    Enclosing,
    /// A function in the current module (or an explicitly qualified name).
    NamedFunction(&'static str),
    /// The compilation root, for generated top-level declarations that are
    /// not caused by another source declaration.
    CompilationRoot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AstNodeMeta {
    pub span: Span,
    pub origin: AstOrigin,
    pub parent: AstParentHint,
}

impl AstNodeMeta {
    pub const fn new(span: Span, origin: AstOrigin) -> Self {
        let parent = match origin {
            AstOrigin::User => AstParentHint::None,
            AstOrigin::Desugared(_)
            | AstOrigin::PrototypeFallback(_)
            | AstOrigin::RuntimeSystem(_) => AstParentHint::Enclosing,
        };
        Self {
            span,
            origin,
            parent,
        }
    }

    pub const fn synthetic(origin: AstOrigin) -> Self {
        Self::new(Span::UNKNOWN, origin)
    }

    /// Metadata for a generated child whose source anchor is inherited from
    /// the user construct that triggered lowering.
    pub const fn inherited(span: Span, origin: AstOrigin) -> Self {
        Self::new(span, origin)
    }

    /// Override the default structural parent for a lowering whose cause is
    /// not its enclosing AST owner.
    pub const fn with_parent(mut self, parent: AstParentHint) -> Self {
        self.parent = parent;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Import {
    pub meta: AstNodeMeta,
    pub path: Vec<String>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Func(FuncDef),
    Module(ModuleDef),
    Type(TypeDef),
    Actor(ActorDef),
    Cap(CapDef),
    Trait(TraitDef),
    Impl(ImplDef),
    ExternBlock(ExternBlock),
    Const {
        meta: AstNodeMeta,
        name: String,
        ty: Option<Type>,
        value: Expr,
        pub_: bool,
    },
    Flow(FlowDef),
    Protocol(ProtocolDef),
    /// Session type declaration: `session Name = !T . ?U . end`
    Session(SessionDef),
}

#[derive(Debug, Clone)]
pub struct ExternBlock {
    pub meta: AstNodeMeta,
    pub abi: String,
    pub funcs: Vec<ExternFunc>,
    /// If true, the compiler wraps all FFI calls in this block with
    /// catch_unwind (Rust panics) and signal handlers (SIGSEGV/SIGABRT).
    pub no_panic: bool,
    /// If true, bypasses the passport-type checker, allowing raw
    /// shared/ref/record/closure types to cross the FFI boundary.
    /// This is an escape hatch for users who need to interface with
    /// C libraries that don't fit the passport-type model.
    pub unsafe_: bool,
    /// SD-3: if true, all functions in this block have errno checking
    /// enabled by default. Individual functions can override with
    /// their own `returns_errno` field.
    pub returns_errno: bool,
}

#[derive(Debug, Clone)]
pub struct ExternFunc {
    pub meta: AstNodeMeta,
    pub name: String,
    pub params: Vec<ExternParam>,
    pub ret: Option<Type>,
    /// Precondition: must hold before the C function is called.
    pub requires: Option<Expr>,
    /// Postcondition: must hold after the C function returns.
    pub ensures: Option<Expr>,
    /// Whether this is a variadic function (e.g., printf).
    pub variadic: bool,
    /// If true, FFI calls to this function are wrapped in catch_unwind
    /// (Rust panics) and signal handlers (C crashes).
    pub no_panic: bool,
    /// SD-3: if true, the FFI runtime captures errno after this call
    /// when the return value indicates an error (negative / -1).
    /// Replaces the legacy `ERRNO_CHECK_FUNC_NAMES` name-guessing.
    pub returns_errno: bool,
}

#[derive(Debug, Clone)]
pub struct ExternParam {
    pub meta: AstNodeMeta,
    pub name: String,
    pub ty: Type,
    pub cap_mode: Option<CapMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapMode {
    Borrow,
    Move,
}

#[derive(Debug, Clone)]
pub struct TraitDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub methods: Vec<TraitMethod>,
    pub generics: Vec<GenericParam>,
}

#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub meta: AstNodeMeta,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
}

#[derive(Debug, Clone)]
pub struct ImplDef {
    pub meta: AstNodeMeta,
    pub generics: Vec<GenericParam>,
    pub trait_name: String,
    pub trait_args: Vec<Type>,
    pub type_name: String,
    pub type_args: Vec<Type>,
    pub methods: Vec<FuncDef>,
}

#[derive(Debug, Clone)]
pub struct CapDef {
    pub meta: AstNodeMeta,
    pub name: String,
    /// None for simple cap, Some(other_cap) for combined cap (A + B)
    pub combined_with: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActorDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub pub_: bool,
    /// v0.31.11: `actor Name runs FlowName` — the actor owns a Flow instance.
    /// Messages decode as Flow events; each turn atomically executes one transition.
    /// When set, `fields` holds only non-business config (mailbox depth, etc.).
    pub runs_flow: Option<String>,
    pub fields: Vec<ActorField>,
    pub methods: Vec<FuncDef>,
}

#[derive(Debug, Clone)]
pub struct ActorField {
    pub meta: AstNodeMeta,
    pub name: String,
    pub ty: Type,
    pub mut_: bool,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub meta: AstNodeMeta,
    pub name: String,
    pub bounds: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FuncDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub pub_: bool,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Block,
    pub where_clause: Vec<WhereClause>,
    pub generics: Vec<GenericParam>,
    pub effects: Vec<String>,
    pub is_comptime: bool,
    pub is_async: bool,
    /// If Some(abi), this function is exported with a C-compatible ABI
    /// (e.g., `extern "C" func foo() { ... }`).
    pub extern_abi: Option<String>,
    /// O(1) contract flags — computed once at parse time, eliminating
    /// per-call body scans in the interpreter hot path (0.31.19 追加 B).
    pub has_requires: bool,
    pub has_ensures: bool,
    pub has_mutate_params: bool,
}

#[derive(Debug, Clone)]
pub struct WhereClause {
    pub meta: AstNodeMeta,
    pub type_param: String,
    pub bounds: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub meta: AstNodeMeta,
    pub name: String,
    pub ty: Type,
    pub mut_: bool,
    pub default_value: Option<Expr>,
    /// v0.29.23: lexical borrow mode for pure-function params.
    /// `None` = by-value (default); `View` = read-only borrow; `Mutate` = exclusive
    /// in-place borrow. Lifetime = call expression (ends on return).
    pub borrow: Option<ParamBorrow>,
}

/// Borrow mode for function parameters (v0.29.23 progressive Typestate borrow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamBorrow {
    /// `view T` — read-only lexical borrow; no writes / no ownership transfer.
    View,
    /// `mutate T` — exclusive in-place borrow; no free / no reallocate / no move-out.
    Mutate,
}

#[derive(Debug, Clone)]
pub struct ModuleDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub pub_: bool,
    pub kind: TypeDefKind,
    pub generics: Vec<GenericParam>,
    pub derives: Vec<String>,
    /// Attributes like #[repr(C)], #[repr(transparent)], etc.
    pub attributes: Vec<TypeAttribute>,
}

/// Type-level attributes (e.g., #[repr(C)])
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeAttribute {
    /// C-compatible memory layout
    ReprC,
    /// Transparent wrapper (same layout as inner type)
    ReprTransparent,
}

#[derive(Debug, Clone)]
pub enum TypeDefKind {
    Alias(Type),
    Newtype(Type),
    Record(Vec<Field>),
    Enum(Vec<Variant>),
    /// C-compatible union type: all fields share the same memory.
    /// Only valid with #[repr(C)] attribute.
    Union(Vec<Field>),
}

#[derive(Debug, Clone)]
pub struct Field {
    pub meta: AstNodeMeta,
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub meta: AstNodeMeta,
    pub name: String,
    pub payload: Option<VariantPayload>,
}

#[derive(Debug, Clone)]
pub enum VariantPayload {
    Tuple(Vec<Type>),
    Record(Vec<Field>),
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub meta: AstNodeMeta,
    pub kind: PatternKind,
}

impl Pattern {
    pub const fn new(meta: AstNodeMeta, kind: PatternKind) -> Self {
        Self { meta, kind }
    }

    pub const fn synthetic(kind: PatternKind, origin: AstOrigin) -> Self {
        Self::new(AstNodeMeta::synthetic(origin), kind)
    }

    /// The binding name if this pattern is a single identifier
    /// (`PatternKind::Variable`). Returns None for tuple/destructuring
    /// patterns — used by for-loop lowering that only binds one name.
    pub fn single_var_name(&self) -> Option<&str> {
        match &self.kind {
            PatternKind::Variable(name) => Some(name.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PatternKind {
    Wildcard,
    Variable(String),
    Literal(Lit),
    Constructor(String, Vec<(String, Pattern)>),
    Tuple(Vec<Pattern>),
    /// Array pattern: [p1, p2, ...]
    Array(Vec<Pattern>),
    /// Slice pattern: [p1, p2, ..rest]
    Slice(Vec<Pattern>, Option<Box<Pattern>>),
}

/// Kind of shared ownership
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedKind {
    /// Thread-safe atomic refcount (`Arc<RwLock<T>>`)
    Shared,
    /// Weak reference (weak upgrade to Shared)
    Weak,
}

pub type Block = Vec<Stmt>;

#[derive(Debug, Clone)]
pub enum Stmt {
    /// Canonical source-aware statement node. The boxed value is its kind;
    /// it is not a second semantic statement. Synthetic constructors remain
    /// representable during migration and report their declared origin.
    Located {
        meta: AstNodeMeta,
        stmt: Box<Stmt>,
    },
    Let {
        pat: Pattern,
        ty: Option<Type>,
        init: Option<Expr>,
        mut_: bool,
        ref_: bool, // let ref x = ... for arena references
    },
    Return(Option<Expr>),
    Break(Option<Expr>),
    Continue,
    Expr(Expr),
    If {
        cond: Expr,
        then_: Block,
        else_: Option<Block>,
    },
    /// v0.34.3: `if let pattern = expr { } else { }` — pattern-match guard.
    /// Symmetric to WhileLet; desugaring to match cannot expose pattern
    /// bindings to the then-block, so it gets its own variant.
    IfLet {
        pat: Pattern,
        init: Expr,
        then_: Block,
        else_: Option<Block>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    /// while let pattern = expr { body }
    WhileLet {
        pat: Pattern,
        init: Expr,
        body: Block,
    },
    /// Infinite loop: loop { break expr }
    Loop(Block),
    /// v0.34.3: `For.var` upgraded from `String` to `Pattern` so
    /// `for (k, v) in map` destructuring works (single ident = Pattern::Variable).
    For {
        var: Pattern,
        iterable: Expr,
        body: Block,
    },
    Block(Block),
    // 0.35.13 (DX backlog #10 trivia-ization): `desc:` / `rule:` demoted to
    // trivia — the parser consumes them inside blocks without producing AST.
    Requires(Expr, Span),
    Ensures(Expr, Span),
    Invariant(Expr, Span),
    Math(Vec<Expr>),
    Assign {
        target: Expr,
        value: Expr,
    },
    /// Arena block for region-based memory management
    Arena(Block),
    /// Unsafe block — allows operations that are normally forbidden
    Unsafe(Block),
    /// v0.34.10a (SD-9): `ieee_float { }` — IEEE 754 escape hatch. Inside,
    /// float results may be NaN/Inf without tripping the finiteness trap.
    IeeeFloat(Block),
    /// Drop a capability
    Drop(Expr),
    /// Defer block — executes when control leaves the current scope (LIFO order)
    Defer(Block),
    /// Shared ownership binding: shared x = expr;
    SharedLet {
        kind: SharedKind,
        name: String,
        ty: Option<Type>,
        init: Expr,
    },
    /// On failure compensation block
    OnFailure(Block),
    /// v0.34.27: `do { }` wrapper removed — language assessment: `do { X }` ≡
    /// bare block `{ X }` (ir/lower.rs:975 same Lexical Scope path, block is an
    /// expression while do never was). Parser rejects `do`; no AST variant.
    /// (was: Do(Block), ~28 points / 15 files; codegen unwrap at compile.rs:1042
    /// proved the equivalence). This also dropped the keyword count 81 → 80,
    /// meeting the 0.1.4 ≤80 goal (golden §1.4).
    /// v0.34.11: `become`/`stay` removed — ADR-001 (golden §1.2): unique
    /// transition terminal is `return S{}`. Parser rejects both keywords;
    /// no AST variants. (was: Become(Expr) / Stay, ~14 files ~30 arms)
    /// v0.34.1: `delegate` removed — 架构修正案条款 2 废止 subflow 委托
    /// (parser 拒绝，clause 2 负测试见 flow_features.rs)。AST variant 已删除。
    /// v0.34.3: `timeout` 字段删除 — 条款 10 废除同步 pinned timeout，
    /// parser 拒绝 timeout 参数，字段恒 None。
    /// Pinned block — pin memory for FFI safety: pinned(expr) |ptr| { ... }
    Pinned {
        expr: Expr,
        var: Option<String>,
        body: Block,
    },
    /// Parallel steps block (parasteps)
    Parasteps(Block),
    // 0.35.13 trivia-ization: `mms {}` super-comment blocks are consumed by
    // the parser without producing AST (MimiSpec intent lives in the .mms
    // toolchain; in .mimi they were always inert super-comments).
    /// Nested function definition inside a block
    Func(FuncDef),
    Ellipsis,
}

impl Stmt {
    pub fn with_meta(self, meta: AstNodeMeta) -> Stmt {
        match self {
            Stmt::Located { stmt, .. } => Stmt::Located { meta, stmt },
            stmt => Stmt::Located {
                meta,
                stmt: Box::new(stmt),
            },
        }
    }

    pub fn synthetic_with_origin(self, origin: AstOrigin) -> Stmt {
        self.with_meta(AstNodeMeta::synthetic(origin))
    }

    pub fn meta(&self) -> Option<AstNodeMeta> {
        match self {
            Stmt::Located { meta, .. } => Some(*meta),
            _ => None,
        }
    }

    pub fn unlocated(&self) -> &Stmt {
        match self {
            Stmt::Located { stmt, .. } => stmt.unlocated(),
            stmt => stmt,
        }
    }

    pub fn unlocated_mut(&mut self) -> &mut Stmt {
        match self {
            Stmt::Located { stmt, .. } => stmt.unlocated_mut(),
            stmt => stmt,
        }
    }

    pub fn into_unlocated(self) -> Stmt {
        match self {
            Stmt::Located { stmt, .. } => stmt.into_unlocated(),
            stmt => stmt,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Expr {
    /// Canonical source-aware expression node. The boxed value is its kind;
    /// it is not a second semantic node. Legacy/synthetic constructors remain
    /// representable during the migration and report no exact metadata.
    Located {
        meta: AstNodeMeta,
        expr: Box<Expr>,
    },
    Literal(Lit),
    Ident(String),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    /// List comprehension: [expr for x in iter if condition]
    Comprehension {
        expr: Box<Expr>,
        var: String,
        iter: Box<Expr>,
        guard: Option<Box<Expr>>,
    },
    Match(Box<Expr>, Vec<MatchArm>),
    Record {
        ty: Option<String>,
        fields: Vec<RecordFieldExpr>,
    },
    /// Block expression `{ stmt; ...; expr }`
    Block(Block),
    /// `?` operator for Result/Option error propagation
    Try(Box<Expr>),
    /// Optional chaining: `x?.y` → `if x is Some(v) { v.y } else { None }`
    /// PA-H3 (audit): currently parsed as `Try(x).y` which is incorrect.
    OptionalChain(Box<Expr>, String),
    /// Spawn a new task/actor
    Spawn(Box<Expr>),
    /// Await a future
    Await(Box<Expr>),
    /// Quote - compile-time AST generation (comptime metaprogramming)
    Quote(Block),
    /// Interpolation inside quote - evaluated at compile time and spliced into AST
    QuoteInterpolate(Box<Expr>),
    /// Comptime block - evaluated at compile time
    Comptime(Block),
    /// TypeOf(expr) - get the runtime type of an expression as a string
    TypeOf(Box<Expr>),
    /// TypeInfo(Type) - get type metadata (fields, variants, methods)
    TypeInfo(Type),
    /// If expression: if cond { then } else { else }
    If {
        cond: Box<Expr>,
        then_: Block,
        else_: Option<Block>,
    },
    /// Lambda/closure expression: fn(params) -> Ret { body }
    Lambda {
        params: Vec<Param>,
        ret: Option<Type>,
        body: Block,
    },
    /// old(expr) - snapshot value at function entry for use in ensures
    Old(Box<Expr>),
    /// Slice expression: expr[start..end]
    #[allow(clippy::enum_variant_names)]
    SliceExpr {
        target: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
    },
    /// Turbofish: `func_name::<Type>(args)` — explicit type instantiation
    Turbofish(String, Vec<Type>, Vec<Expr>),
    /// Numeric tuple field access: t.0, t.1, etc.
    TupleIndex(Box<Expr>, usize),
    /// Arena block expression: arena { stmts; expr }
    Arena(Block),
    /// Map literal: {"key1": value1, "key2": value2}
    MapLiteral {
        entries: Vec<(Expr, Expr)>,
    },
    /// Set literal: {1, 2, 3}
    SetLiteral(Vec<Expr>),
    /// Named argument in function call: f(x = 5)
    NamedArg(String, Box<Expr>),
    /// Type cast: expr as Type
    Cast(Box<Expr>, Type),
}

impl Expr {
    pub fn with_meta(self, meta: AstNodeMeta) -> Expr {
        match self {
            Expr::Located { expr, .. } => Expr::Located { meta, expr },
            expr => Expr::Located {
                meta,
                expr: Box::new(expr),
            },
        }
    }

    pub fn synthetic_with_origin(self, origin: AstOrigin) -> Expr {
        self.with_meta(AstNodeMeta::synthetic(origin))
    }

    pub fn meta(&self) -> Option<AstNodeMeta> {
        match self {
            Expr::Located { meta, .. } => Some(*meta),
            _ => None,
        }
    }

    pub fn unlocated(&self) -> &Expr {
        match self {
            Expr::Located { expr, .. } => expr.unlocated(),
            expr => expr,
        }
    }

    pub fn unlocated_mut(&mut self) -> &mut Expr {
        match self {
            Expr::Located { expr, .. } => expr.unlocated_mut(),
            expr => expr,
        }
    }

    pub fn into_unlocated(self) -> Expr {
        match self {
            Expr::Located { expr, .. } => expr.into_unlocated(),
            expr => expr,
        }
    }

    pub fn call(self, args: Vec<Expr>) -> Expr {
        Expr::Call(Box::new(self), args)
    }

    pub fn field(self, name: impl Into<String>) -> Expr {
        Expr::Field(Box::new(self), name.into())
    }

    pub fn index(self, index: Expr) -> Expr {
        Expr::Index(Box::new(self), Box::new(index))
    }

    pub fn tuple_index(self, idx: usize) -> Expr {
        Expr::TupleIndex(Box::new(self), idx)
    }

    pub fn try_expr(self) -> Expr {
        Expr::Try(Box::new(self))
    }

    pub fn with_slice(self, start: Option<Box<Expr>>, end: Option<Box<Expr>>) -> Expr {
        Expr::SliceExpr {
            target: Box::new(self),
            start,
            end,
        }
    }

    pub fn unary(self, op: UnOp) -> Expr {
        Expr::Unary(op, Box::new(self))
    }

    pub fn binary(self, op: BinOp, rhs: Expr) -> Expr {
        Expr::Binary(op, Box::new(self), Box::new(rhs))
    }
}

#[derive(Debug, Clone)]
pub struct RecordFieldExpr {
    pub meta: AstNodeMeta,
    pub name: String,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub meta: AstNodeMeta,
    pub pat: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Debug, Clone)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    FString(Vec<FStringPart>),
    Unit,
}

/// A part of an f-string: either literal text or an interpolation expression
#[derive(Debug, Clone)]
pub enum FStringPart {
    Text(String),
    Interp(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    EqCmp,
    NeCmp,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Range,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Ref,
    RefMut,
    Deref,
}

#[derive(Debug, Clone)]
pub enum Type {
    /// Canonical source-aware type node. Metadata is intentionally excluded
    /// from semantic equality: two structurally equal types remain equal even
    /// when they were written at different source locations.
    Located {
        meta: AstNodeMeta,
        ty: Box<Type>,
    },
    Name(String, Vec<Type>),
    /// Reference type: &'lt T (lifetime is optional)
    Ref(Option<String>, Box<Type>),
    /// Mutable reference: &'lt mut T (lifetime is optional)
    RefMut(Option<String>, Box<Type>),
    Option(Box<Type>),
    Result(Box<Type>, Box<Type>),
    Tuple(Vec<Type>),
    Func(Vec<Type>, Box<Type>),
    /// C-compatible function pointer: extern "C" fn(Args...) -> Ret
    ExternFunc(Vec<Type>, Box<Type>),
    /// C buffer type with automatic memory management (malloc/free)
    CBuffer(Box<Type>),
    /// Capability type for linear capabilities
    Cap(String),
    /// Atomic capability component produced by `combined.split()` — cannot be
    /// split again (H5, audit 2026-08-03). Distinct from `Cap` so the checker
    /// can reject nested splits at compile time instead of diverging between
    /// the bytecode VM (runtime E0800) and codegen (E0700 on the opaque i64
    /// handle).
    CapAtom(String),
    /// Shared ownership (atomic refcount, thread-safe)
    Shared(Box<Type>),
    /// Weak reference from shared
    Weak(Box<Type>),
    /// Newtype wrapper for strong type isolation (name, inner type)
    Newtype(String, Box<Type>),
    /// Fixed-size array type: `[T; n]`
    Array(Box<Type>, usize),
    /// Slice type: `&[T]`
    Slice(Box<Type>),
    /// impl Trait return type — opaque type implementing listed traits
    ImplTrait(Vec<String>),
    /// dyn Trait — runtime trait object (fat pointer: data + vtable)
    DynTrait(Vec<String>),
    /// Raw C pointer: *T
    RawPtr(Box<Type>),
    /// Raw mutable C pointer: *mut T
    RawPtrMut(Box<Type>),
    /// Nothing type (unreachable / error residual). Never writable from source
    /// (the `nothing` keyword was culled in 0.35.39); produced internally as
    /// the canonical residual for uninhabited payloads.
    Nothing,
    /// Inferred type: `_` — let the compiler determine the type
    Infer,
    /// Type inference variable (for unification engine)
    TypeVar(u32),
    /// Polymorphic type: forall T. Body
    ForAll(Vec<String>, Box<Type>),
    /// 0.31.42: Poison type — injected when type inference fails.
    ///
    /// TyErr unifies with *any* type (poison semantics): once a type error
    /// is detected, TyErr replaces the offending type to suppress cascading
    /// downstream errors. Displayed as `«error»` in diagnostics.
    ///
    /// Invariant: TyErr must never reach codegen. The checker's zonk phase
    /// rejects residual TyErr (same as residual TypeVar/Infer).
    TyErr,
}

impl Type {
    /// Attach exact source/provenance metadata to this type node.
    /// Reattaching metadata replaces the outer annotation instead of nesting
    /// redundant `Located` wrappers.
    pub fn with_meta(self, meta: AstNodeMeta) -> Type {
        match self {
            Type::Located { ty, .. } => Type::Located { meta, ty },
            ty => Type::Located {
                meta,
                ty: Box::new(ty),
            },
        }
    }

    /// Attach provenance to a synthesized type that has no exact user span.
    pub fn synthetic_with_origin(self, origin: AstOrigin) -> Type {
        self.with_meta(AstNodeMeta::synthetic(origin))
    }

    /// Replace provenance on this type and every nested type node.
    ///
    /// Lowering passes use this when a user type is copied into a generated
    /// declaration. Keeping the original nested `User` annotations would make
    /// the generated declaration appear to contain user-written children even
    /// though the entire type tree belongs to one lowering rule.
    pub fn deep_reorigin(self, meta: AstNodeMeta) -> Type {
        let ty = match self.into_unlocated() {
            Type::Name(name, args) => Type::Name(
                name,
                args.into_iter()
                    .map(|arg| arg.deep_reorigin(meta))
                    .collect(),
            ),
            Type::Ref(lifetime, inner) => {
                Type::Ref(lifetime, Box::new((*inner).deep_reorigin(meta)))
            }
            Type::RefMut(lifetime, inner) => {
                Type::RefMut(lifetime, Box::new((*inner).deep_reorigin(meta)))
            }
            Type::Option(inner) => Type::Option(Box::new((*inner).deep_reorigin(meta))),
            Type::Result(ok, err) => Type::Result(
                Box::new((*ok).deep_reorigin(meta)),
                Box::new((*err).deep_reorigin(meta)),
            ),
            Type::Tuple(items) => Type::Tuple(
                items
                    .into_iter()
                    .map(|item| item.deep_reorigin(meta))
                    .collect(),
            ),
            Type::Func(params, ret) => Type::Func(
                params
                    .into_iter()
                    .map(|param| param.deep_reorigin(meta))
                    .collect(),
                Box::new((*ret).deep_reorigin(meta)),
            ),
            Type::ExternFunc(params, ret) => Type::ExternFunc(
                params
                    .into_iter()
                    .map(|param| param.deep_reorigin(meta))
                    .collect(),
                Box::new((*ret).deep_reorigin(meta)),
            ),
            Type::CBuffer(inner) => Type::CBuffer(Box::new((*inner).deep_reorigin(meta))),
            Type::Cap(name) => Type::Cap(name),
            Type::CapAtom(name) => Type::CapAtom(name),
            Type::Shared(inner) => Type::Shared(Box::new((*inner).deep_reorigin(meta))),
            Type::Weak(inner) => Type::Weak(Box::new((*inner).deep_reorigin(meta))),
            Type::Newtype(name, inner) => {
                Type::Newtype(name, Box::new((*inner).deep_reorigin(meta)))
            }
            Type::Nothing => Type::Nothing,
            Type::TyErr => Type::TyErr,
            Type::Array(inner, size) => Type::Array(Box::new((*inner).deep_reorigin(meta)), size),
            Type::Slice(inner) => Type::Slice(Box::new((*inner).deep_reorigin(meta))),
            Type::ImplTrait(traits) => Type::ImplTrait(traits),
            Type::DynTrait(traits) => Type::DynTrait(traits),
            Type::RawPtr(inner) => Type::RawPtr(Box::new((*inner).deep_reorigin(meta))),
            Type::RawPtrMut(inner) => Type::RawPtrMut(Box::new((*inner).deep_reorigin(meta))),
            Type::Infer => Type::Infer,
            Type::TypeVar(id) => Type::TypeVar(id),
            Type::ForAll(params, body) => {
                Type::ForAll(params, Box::new((*body).deep_reorigin(meta)))
            }
            Type::Located { .. } => unreachable!("Type::into_unlocated returned Located"),
        };
        ty.with_meta(meta)
    }

    /// Return exact metadata when this node is source/provenance annotated.
    pub fn meta(&self) -> Option<AstNodeMeta> {
        match self {
            Type::Located { meta, .. } => Some(*meta),
            _ => None,
        }
    }

    /// Borrow the semantic type kind, transparently skipping annotations.
    pub fn unlocated(&self) -> &Type {
        match self {
            Type::Located { ty, .. } => ty.unlocated(),
            ty => ty,
        }
    }

    /// Mutably borrow the semantic type kind, transparently skipping
    /// annotations.
    pub fn unlocated_mut(&mut self) -> &mut Type {
        match self {
            Type::Located { ty, .. } => ty.unlocated_mut(),
            ty => ty,
        }
    }

    /// Consume annotations and return the semantic type kind.
    pub fn into_unlocated(self) -> Type {
        match self {
            Type::Located { ty, .. } => ty.into_unlocated(),
            ty => ty,
        }
    }
}

impl PartialEq for Type {
    fn eq(&self, other: &Self) -> bool {
        use Type::*;

        match (self.unlocated(), other.unlocated()) {
            (Name(a_name, a_args), Name(b_name, b_args)) => a_name == b_name && a_args == b_args,
            (Ref(a_lt, a), Ref(b_lt, b)) | (RefMut(a_lt, a), RefMut(b_lt, b)) => {
                a_lt == b_lt && a == b
            }
            (Option(a), Option(b))
            | (CBuffer(a), CBuffer(b))
            | (Shared(a), Shared(b))
            | (Weak(a), Weak(b))
            | (Slice(a), Slice(b))
            | (RawPtr(a), RawPtr(b))
            | (RawPtrMut(a), RawPtrMut(b)) => a == b,
            (Result(a_ok, a_err), Result(b_ok, b_err)) => a_ok == b_ok && a_err == b_err,
            (Tuple(a), Tuple(b)) => a == b,
            (Func(a_args, a_ret), Func(b_args, b_ret))
            | (ExternFunc(a_args, a_ret), ExternFunc(b_args, b_ret)) => {
                a_args == b_args && a_ret == b_ret
            }
            (Cap(a), Cap(b)) => a == b,
            (CapAtom(a), CapAtom(b)) => a == b,
            (Newtype(a_name, a), Newtype(b_name, b)) => a_name == b_name && a == b,
            (Nothing, Nothing) | (Infer, Infer) | (TyErr, TyErr) => true,
            (Array(a, a_len), Array(b, b_len)) => a_len == b_len && a == b,
            (ImplTrait(a), ImplTrait(b)) | (DynTrait(a), DynTrait(b)) => a == b,
            (TypeVar(a), TypeVar(b)) => a == b,
            (ForAll(a_params, a), ForAll(b_params, b)) => a_params == b_params && a == b,
            // `unlocated` above recursively removes every outer annotation,
            // so reaching `Located` here would indicate a broken invariant.
            (Located { .. }, _) | (_, Located { .. }) => {
                unreachable!("unlocated() must strip all Located wrappers before structural_eq")
            }
            _ => false,
        }
    }
}

impl Eq for Type {}

// ── Flow/state/transition types ─────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FlowDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub pub_: bool,
    pub generics: Vec<GenericParam>,
    pub annotations: Vec<FlowAnnotation>,
    pub states: Vec<StateDef>,
    pub transitions: Vec<TransitionDef>,
    /// Protocol names this flow implements (e.g., `Sensor`)
    pub impl_protocols: Vec<String>,
    /// Fields declared as `persistent` — survive Fault and recover
    pub persistent_fields: Vec<String>,
    /// v0.34.1: `transactional_fields` / `metadata_shadow_fields` removed —
    /// 架构修正案条款 3 废止 WAL/@transactional（曾随 0.1.3 RC 确认）。
    /// v0.31.10: Per-Flow typed Fault — `fault ErrorType` declaration in flow body.
    /// When set, the injected Fault state carries an additional `error: ErrorType` field.
    pub fault_type: Option<Type>,
}

#[derive(Debug, Clone)]
pub struct FlowAnnotation {
    pub meta: AstNodeMeta,
    pub kind: FlowAnnotationKind,
}

impl PartialEq for FlowAnnotation {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Eq for FlowAnnotation {}

impl FlowAnnotation {
    pub const fn new(meta: AstNodeMeta, kind: FlowAnnotationKind) -> Self {
        Self { meta, kind }
    }

    pub const fn synthetic(kind: FlowAnnotationKind, origin: AstOrigin) -> Self {
        Self::new(AstNodeMeta::synthetic(origin), kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowAnnotationKind {
    MailboxDepth(usize),
    MaxChildren(usize),
    /// v0.31.10: Sparse transition graph — skip fallback injection for
    /// missing (state, event) pairs. Undefined events are compile-time errors
    /// instead of auto-routing to Fault.
    /// v0.31.25: Now the DEFAULT behavior. Kept for backward compatibility
    /// (accepted but redundant).
    Sparse,
    // M7 (audit-syntax 2026-08-03): the `Dense` variant was removed — it was
    // unreachable from user code (the parser rejects `@dense` per amendment
    // clause 1, sparse-irreversible) and only synthesized by progressive.rs,
    // which now emits Sparse. No consumer ever branched on Dense.
}

#[derive(Debug, Clone)]
pub struct StateDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub payload: Option<Vec<Field>>,
}

#[derive(Debug, Clone)]
pub struct TransitionDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub from_state: String,
    pub params: Vec<Param>,
    /// Target states listed in the transition signature (e.g., `-> Active | OverloadWarning`)
    pub to_states: Vec<String>,
    /// FLOW-TURN-001: declared rollback error type (`fails E`).
    /// When present, `?` in the transition body lowers to `Rejected` —
    /// the draft is discarded and the source generation is returned
    /// to the caller alongside the typed error.
    pub fails: Option<Type>,
    /// Transition body — requires a `do { }` block
    pub body: Option<Block>,
    /// True when this transition was injected by the compiler (system verbs:
    /// reset/recover/peer_fault/ffi_crash, or N×M fallback in @dense mode).
    /// User-written transitions always have `false`.
    pub is_fallback: bool,
    /// v0.29.42: True for injected FFI_Pinned enter/exit/crash transitions.
    /// User-written transitions always have `false`.
    pub is_ffi_pinned: bool,
}

#[derive(Debug, Clone)]
pub struct ProtocolDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub states: Vec<ProtocolStateDef>,
    pub transitions: Vec<ProtocolTransitionDef>,
}

/// Top-level session type alias: `session Name = SessionTypeExpr`.
#[derive(Debug, Clone)]
pub struct SessionDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub pub_: bool,
    pub body: SessionType,
}

/// Linear session type expression (v0.29.19 skeleton).
///
/// Syntax (prefix actions, `.` sequencing, `end` termination):
/// ```text
/// session S = !i32 . ?string . end
/// session T = dual(S)
/// ```
#[derive(Debug, Clone)]
pub enum SessionType {
    /// Source/provenance wrapper. Semantic session operations ignore it.
    Located {
        meta: AstNodeMeta,
        session: Box<SessionType>,
    },
    /// Send a value of type `T`, then continue as `cont`: `!T . cont`
    Send(Type, Box<SessionType>),
    /// Receive a value of type `T`, then continue as `cont`: `?T . cont`
    Recv(Type, Box<SessionType>),
    /// Dual of another session: `dual(S)` or `dual(session-expr)`
    Dual(Box<SessionType>),
    /// Named session reference (after `session Name = ...` declaration)
    Name(String),
    /// Protocol termination: `end`
    End,
}

impl SessionType {
    pub fn with_meta(self, meta: AstNodeMeta) -> Self {
        match self {
            Self::Located { session, .. } => Self::Located { meta, session },
            session => Self::Located {
                meta,
                session: Box::new(session),
            },
        }
    }

    pub fn synthetic_with_origin(self, origin: AstOrigin) -> Self {
        self.with_meta(AstNodeMeta::synthetic(origin))
    }

    pub fn meta(&self) -> Option<AstNodeMeta> {
        match self {
            Self::Located { meta, .. } => Some(*meta),
            _ => None,
        }
    }

    pub fn unlocated(&self) -> &Self {
        match self {
            Self::Located { session, .. } => session.unlocated(),
            session => session,
        }
    }

    pub fn into_unlocated(self) -> Self {
        match self {
            Self::Located { session, .. } => session.into_unlocated(),
            session => session,
        }
    }
}

impl PartialEq for SessionType {
    fn eq(&self, other: &Self) -> bool {
        match (self.unlocated(), other.unlocated()) {
            (Self::Send(a_ty, a_cont), Self::Send(b_ty, b_cont))
            | (Self::Recv(a_ty, a_cont), Self::Recv(b_ty, b_cont)) => {
                a_ty == b_ty && a_cont == b_cont
            }
            (Self::Dual(a), Self::Dual(b)) => a == b,
            (Self::Name(a), Self::Name(b)) => a == b,
            (Self::End, Self::End) => true,
            (Self::Located { .. }, _) | (_, Self::Located { .. }) => {
                unreachable!("unlocated() must strip all Located wrappers before SessionType eq")
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProtocolStateDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub payload_name: Option<String>,
    pub payload_type: Option<Type>,
}

#[derive(Debug, Clone)]
pub struct ProtocolTransitionDef {
    pub meta: AstNodeMeta,
    pub name: String,
    pub from_state: String,
    pub to_state: String,
}
