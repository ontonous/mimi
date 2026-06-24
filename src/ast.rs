use crate::span::Span;

#[derive(Debug, Clone)]
pub struct File {
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub path: Vec<String>,
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
}

#[derive(Debug, Clone)]
pub struct ExternBlock {
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
}

#[derive(Debug, Clone)]
pub struct ExternFunc {
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
}

#[derive(Debug, Clone)]
pub struct ExternParam {
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
    pub name: String,
    pub methods: Vec<TraitMethod>,
    pub generics: Vec<GenericParam>,
}

#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
}

#[derive(Debug, Clone)]
pub struct ImplDef {
    pub generics: Vec<GenericParam>,
    pub trait_name: String,
    pub trait_args: Vec<Type>,
    pub type_name: String,
    pub type_args: Vec<Type>,
    pub methods: Vec<FuncDef>,
}

#[derive(Debug, Clone)]
pub struct CapDef {
    pub name: String,
    /// None for simple cap, Some(other_cap) for combined cap (A + B)
    pub combined_with: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActorDef {
    pub name: String,
    pub pub_: bool,
    pub fields: Vec<ActorField>,
    pub methods: Vec<FuncDef>,
}

#[derive(Debug, Clone)]
pub struct ActorField {
    pub name: String,
    pub ty: Type,
    pub mut_: bool,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub bounds: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FuncDef {
    pub name: String,
    pub pub_: bool,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Block,
    pub where_clause: Option<WhereClause>,
    pub generics: Vec<GenericParam>,
    pub effects: Vec<String>,
    pub is_comptime: bool,
    pub is_async: bool,
    /// If Some(abi), this function is exported with a C-compatible ABI
    /// (e.g., `extern "C" func foo() { ... }`).
    pub extern_abi: Option<String>,
    /// Source position (line, col) from the `func` keyword
    pub pos: (usize, usize),
}

#[derive(Debug, Clone)]
pub struct WhereClause {
    pub type_param: String,
    pub bounds: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub mut_: bool,
}

#[derive(Debug, Clone)]
pub struct ModuleDef {
    pub name: String,
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
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
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub payload: Option<VariantPayload>,
}

#[derive(Debug, Clone)]
pub enum VariantPayload {
    Tuple(Vec<Type>),
    Record(Vec<Field>),
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard,
    Variable(String),
    Literal(Lit),
    Constructor(String, Vec<Pattern>),
    Tuple(Vec<Pattern>),
    /// Array pattern: [p1, p2, ...]
    Array(Vec<Pattern>),
    /// Slice pattern: [p1, p2, ..rest]
    Slice(Vec<Pattern>, Option<Box<Pattern>>),
}

/// Kind of shared ownership
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedKind {
    /// Thread-safe atomic refcount (Arc<RwLock<T>>)
    Shared,
    /// Single-thread non-atomic (Rc<RefCell<T>>)
    LocalShared,
    /// Weak reference (weak upgrade to Shared)
    Weak,
    /// Weak reference (weak upgrade to LocalShared)
    WeakLocal,
}

pub type Block = Vec<Stmt>;

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        pat: Pattern,
        ty: Option<Type>,
        init: Option<Expr>,
        mut_: bool,
        ref_: bool,  // let ref x = ... for arena references
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
    While {
        cond: Expr,
        body: Block,
    },
    /// Infinite loop: loop { break expr }
    Loop(Block),
    For {
        var: String,
        iterable: Expr,
        body: Block,
    },
    Block(Block),
    Desc(String, Span),
    Rule(String, Span),
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
    /// Drop a capability
    Drop(Expr),
    /// Shared ownership binding: shared x = expr;
    SharedLet {
        kind: SharedKind,
        name: String,
        ty: Option<Type>,
        init: Expr,
    },
    /// On failure compensation block
    OnFailure(Block),
    /// Parallel steps block (parasteps)
    Parasteps(Block),
    /// mms {} super-comment block containing MimiSpec intent
    MmsBlock {
        content: String,
        ast: Option<mimispec::ast::File>,
        span: crate::span::Span,
    },
    /// alloc(Kind) { ... } block using a specific allocator
    Alloc {
        kind: AllocKind,
        body: Block,
    },
    Ellipsis,
}

#[derive(Debug, Clone)]
pub enum Expr {
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
    /// Range expression: start..end
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
    },
    /// Turbofish: func_name::<Type>(args) — explicit type instantiation
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
}

impl Expr {
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
        Expr::SliceExpr { target: Box::new(self), start, end }
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
    pub name: String,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
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
    Assign,
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
    /// Shared ownership (atomic refcount, thread-safe)
    Shared(Box<Type>),
    /// Local shared ownership (non-atomic, single-thread)
    LocalShared(Box<Type>),
    /// Weak reference from shared
    Weak(Box<Type>),
    /// Weak reference from local_shared
    WeakLocal(Box<Type>),
    /// Newtype wrapper for strong type isolation (name, inner type)
    Newtype(String, Box<Type>),
    /// Nothing type (unreachable / error type)
    Nothing,
    /// Allocator type for custom memory allocation
    Allocator,
    /// Fixed-size array type: [T; n]
    Array(Box<Type>, usize),
    /// Slice type: &[T]
    Slice(Box<Type>),
    /// impl Trait return type — opaque type implementing listed traits
    ImplTrait(Vec<String>),
    /// dyn Trait — runtime trait object (fat pointer: data + vtable)
    DynTrait(Vec<String>),
    /// Raw C pointer: *T
    RawPtr(Box<Type>),
    /// Raw mutable C pointer: *mut T
    RawPtrMut(Box<Type>),
    /// C-compatible shared ownership handle: c_shared T
    CShared(Box<Type>),
    /// C-compatible immutable borrow: c_borrow T
    CBorrow(Box<Type>),
    /// C-compatible mutable borrow: c_borrow_mut T
    CBorrowMut(Box<Type>),
    /// Raw string ownership transfer: raw string (C must free via mimi_string_free_raw)
    RawString,
    /// Inferred type: `_` — let the compiler determine the type
    Infer,
}

/// Kind of allocator for alloc blocks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocKind {
    /// System default allocator (malloc/free)
    System,
    /// Arena region allocator (bulk free)
    Arena,
    /// Bump allocator (monotonic, fast)
    Bump,
}
