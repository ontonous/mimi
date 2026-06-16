/// 意图锁后缀（直接复用 mimispec 的语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Commitment {
    #[default]
    None,
    Question,
    QuestionQuestion,
    Locked,
    StrongLocked,
    LockedQuestion,
    StrongLockedQuestion,
    LockedQuestionQuestion,
    StrongLockedQuestionQuestion,
}

impl Commitment {
    pub fn is_locked(&self) -> bool {
        matches!(
            self,
            Self::Locked
                | Self::StrongLocked
                | Self::LockedQuestion
                | Self::StrongLockedQuestion
                | Self::LockedQuestionQuestion
                | Self::StrongLockedQuestionQuestion
        )
    }
}

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
    Rule(String),
    Desc(String),
}

#[derive(Debug, Clone)]
pub struct CapDef {
    pub name: String,
    pub commitment: Commitment,
    /// None for simple cap, Some(other_cap) for combined cap (A + B)
    pub combined_with: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActorDef {
    pub name: String,
    pub commitment: Commitment,
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
pub struct FuncDef {
    pub name: String,
    pub commitment: Commitment,
    pub pub_: bool,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Block,
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
    pub commitment: Commitment,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub name: String,
    pub commitment: Commitment,
    pub pub_: bool,
    pub kind: TypeDefKind,
}

#[derive(Debug, Clone)]
pub enum TypeDefKind {
    Alias(Type),
    Newtype(Type),
    Record(Vec<Field>),
    Enum(Vec<Variant>),
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
    For {
        var: String,
        iterable: Expr,
        body: Block,
    },
    Block(Block),
    Desc(String),
    Requires(Expr),
    Ensures(Expr),
    Math(Vec<Expr>),
    Assign {
        target: Expr,
        value: Expr,
    },
    /// Arena block for region-based memory management
    Arena(Block),
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
    Match(Box<Expr>, Vec<MatchArm>),
    Record {
        ty: Option<String>,
        fields: Vec<RecordFieldExpr>,
    },
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
    /// Lambda/closure expression: fn(params) -> Ret { body }
    Lambda {
        params: Vec<Param>,
        ret: Option<Type>,
        body: Block,
    },
    /// old(expr) - snapshot value at function entry for use in ensures
    Old(Box<Expr>),
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
    Unit,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Ref,
    RefMut,
}

#[derive(Debug, Clone)]
pub enum Type {
    Name(String, Vec<Type>),
    Ref(Box<Type>),
    RefMut(Box<Type>),
    Option(Box<Type>),
    Result(Box<Type>, Box<Type>),
    Tuple(Vec<Type>),
    Func(Vec<Type>, Box<Type>),
    /// Capability type for linear capabilities
    Cap(String),
    /// Shared ownership (atomic refcount, thread-safe)
    Shared(Box<Type>),
    /// Local shared ownership (non-atomic, single-thread)
    LocalShared(Box<Type>),
    /// Weak reference from shared
    Weak(Box<Type>),
    /// Newtype wrapper for strong type isolation (name, inner type)
    Newtype(String, Box<Type>),
    /// Nothing type (unreachable / error type)
    Nothing,
}
