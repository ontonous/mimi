use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

/// Compute the Levenshtein edit distance between two strings.
#[allow(clippy::needless_range_loop)]
fn edit_distance(a: &str, b: &str) -> usize {
    let a_len = a.len();
    let b_len = b.len();
    let mut matrix = vec![vec![0usize; b_len + 1]; a_len + 1];

    for i in 0..=a_len {
        matrix[i][0] = i;
    }
    for j in 0..=b_len {
        matrix[0][j] = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a.as_bytes()[i - 1] == b.as_bytes()[j - 1] { 0 } else { 1 };
            matrix[i][j] = std::cmp::min(
                std::cmp::min(
                    matrix[i - 1][j] + 1,      // deletion
                    matrix[i][j - 1] + 1,      // insertion
                ),
                matrix[i - 1][j - 1] + cost,  // substitution
            );
        }
    }

    matrix[a_len][b_len]
}

/// Find the closest matching name from a list of candidates.
/// Returns the best match if its edit distance is <= max_distance.
fn suggest_name(name: &str, candidates: &[String], max_distance: usize) -> Option<String> {
    let mut best: Option<(String, usize)> = None;
    for candidate in candidates {
        let dist = edit_distance(name, candidate);
        if dist <= max_distance && dist > 0 {
            match &best {
                Some((_, best_dist)) if dist < *best_dist => {
                    best = Some((candidate.clone(), dist));
                }
                None => {
                    best = Some((candidate.clone(), dist));
                }
                _ => {}
            }
        }
    }
    best.map(|(name, _)| name)
}

pub fn check(file: &File) -> Result<(), Vec<Diagnostic>> {
    let mut checker = Checker::new(file);
    checker.check()
}

pub fn check_strict(file: &File) -> Result<(), Vec<Diagnostic>> {
    let mut checker = Checker::new(file);
    checker.strict = true;
    checker.check()
}

/// Verify that MMS rule attachments are consistent.
/// Rules must be attached to a following entity; orphan rules are errors.
pub fn verify_rules(file: &File) -> Vec<String> {
    let mut errors = Vec::new();
    for item in &file.items {
        match item {
            Item::Func(func) => {
                verify_rules_in_block(&func.body, &mut errors, &func.name);
            }
            Item::Module(module) => {
                for item in &module.items {
                    if let Item::Func(func) = item {
                        verify_rules_in_block(&func.body, &mut errors, &func.name);
                    }
                }
            }
            _ => {}
        }
    }
    errors
}

fn verify_rules_in_block(block: &[Stmt], errors: &mut Vec<String>, context: &str) {
    let mut last_was_rule = false;
    let mut rule_pos = String::new();
    for stmt in block {
        match stmt {
            Stmt::Desc(text) if text.starts_with("rule:") => {
                // Rule must be followed by requires/ensures or a block that contains them.
                // For now, flag any consecutive rules without intervening contract.
                if last_was_rule {
                    errors.push(format!(
                        "consecutive rules without attached contract in '{}': '{}'",
                        context, text
                    ));
                }
                last_was_rule = true;
                rule_pos = text.clone();
            }
            Stmt::Requires(_, _) | Stmt::Ensures(_, _) => {
                last_was_rule = false;
            }
            Stmt::Block(inner) => {
                verify_rules_in_block(inner, errors, context);
                // A block after a rule potentially contains the contract
                if last_was_rule {
                    last_was_rule = false;
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => {
                verify_rules_in_block(body, errors, context);
                last_was_rule = false;
            }
            Stmt::If { then_, else_, .. } => {
                verify_rules_in_block(then_, errors, context);
                if let Some(else_) = else_ {
                    verify_rules_in_block(else_, errors, context);
                }
                last_was_rule = false;
            }
            _ => {
                last_was_rule = false;
            }
        }
    }
    if last_was_rule {
        errors.push(format!(
            "orphan rule without attached contract at end of '{}': '{}'",
            context, rule_pos
        ));
    }
}

/// Track borrow state with location information for precise diagnostics.
#[derive(Debug, Clone)]
enum BorrowState {
    Unborrowed,
    BorrowedImm { span: Span },
    BorrowedMut { span: Span },
}

impl BorrowState {
    #[allow(dead_code)]
    fn is_borrowed(&self) -> bool {
        !matches!(self, BorrowState::Unborrowed)
    }
}

struct Checker<'a> {
    file: &'a File,
    errors: Vec<Diagnostic>,
    #[allow(dead_code)]
    warnings: Vec<Diagnostic>,
    funcs: HashMap<String, (Vec<Type>, Type)>,
    aliases: HashMap<String, Type>,
    types: HashMap<String, TypeDef>,
    /// Track newtype definitions: name -> inner type (unresolved)
    newtypes: HashMap<String, Type>,
    /// Track linear capabilities in scope: name -> consumed
    cap_vars: Vec<HashMap<String, bool>>,
    /// Track borrow state of variables: name -> borrow state
    borrows: Vec<HashMap<String, BorrowState>>,
    /// Track trait definitions: trait_name -> list of method names
    traits: HashMap<String, Vec<String>>,
    /// Track trait generic params: trait_name -> list of generic param names
    trait_generics: HashMap<String, Vec<String>>,
    /// Track trait implementations: (trait_name, type_name) -> list of method names
    impls: HashMap<(String, String), Vec<String>>,
    /// Track where clauses for functions: func_name -> (type_param, bounds)
    where_clauses: HashMap<String, (String, Vec<String>)>,
    /// Track effects for functions: func_name -> list of effect names
    func_effects: HashMap<String, Vec<String>>,
    /// Track available effects in current scope
    available_effects: Vec<HashMap<String, bool>>,
    /// Strict mode: enforce $$ lock semantics
    strict: bool,
    /// Track variable scopes for shadowing detection
    var_scopes: Vec<HashMap<String, usize>>,
    /// Track mutable variables: name -> is_mut
    mut_vars: Vec<HashMap<String, bool>>,
    /// Track generic parameters per function: func_name -> generic params
    func_generics: HashMap<String, Vec<GenericParam>>,
    /// Track generic parameters per type def: type_name -> generic params
    type_generics: HashMap<String, Vec<GenericParam>>,
    /// Track methods available on types via traits: type_name -> list of (trait_name, method_name)
    type_methods: HashMap<String, Vec<(String, String)>>,
    /// Track trait method signatures: (trait_name, method_name) -> (param_types, return_type)
    trait_method_sigs: HashMap<(String, String), (Vec<Type>, Type)>,
    /// Track imported module names (from `use` statements)
    use_imports: Vec<String>,
    /// Track current module path for qualified names
    module_path: Vec<String>,
    /// Track loop nesting depth for break/continue validation
    loop_depth: usize,
    /// Track generic parameters in scope while checking signatures
    generic_scope: Vec<String>,
    /// Current item/function line-col for fallback error positioning
    current_line: usize,
    current_col: usize,
}

mod check_stmt;
mod infer_expr;

impl<'a> Checker<'a> {
    fn new(file: &'a File) -> Self {
        Self {
            file,
            errors: Vec::new(),
            warnings: Vec::new(),
            funcs: HashMap::new(),
            aliases: HashMap::new(),
            types: HashMap::new(),
            newtypes: HashMap::new(),
            cap_vars: vec![HashMap::new()],
            borrows: vec![HashMap::new()],
            traits: HashMap::new(),
            trait_generics: HashMap::new(),
            impls: HashMap::new(),
            where_clauses: HashMap::new(),
            func_effects: HashMap::new(),
            available_effects: vec![HashMap::new()],
            strict: false,
            var_scopes: vec![HashMap::new()],
            mut_vars: vec![HashMap::new()],
            func_generics: HashMap::new(),
            type_generics: HashMap::new(),
            type_methods: HashMap::new(),
            trait_method_sigs: HashMap::new(),
            use_imports: Vec::new(),
            module_path: Vec::new(),
            loop_depth: 0,
            generic_scope: Vec::new(),
            current_line: 0,
            current_col: 0,
        }
    }

    /// Set the current position for fallback error spans.
    pub(crate) fn set_pos(&mut self, line: usize, col: usize) {
        self.current_line = line;
        self.current_col = col;
    }

    fn check(&mut self) -> Result<(), Vec<Diagnostic>> {
        self.collect_decls();
        for item in &self.file.items {
            self.check_item(item);
        }
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(std::mem::take(&mut self.errors))
        }
    }

    fn emit(&mut self, msg: impl Into<String>) {
        let span = Span::single(self.current_line, self.current_col);
        self.errors.push(Diagnostic::error(msg, span));
    }

    pub(crate) fn emit_at(&mut self, msg: impl Into<String>, line: usize, col: usize) {
        self.errors.push(Diagnostic::error(msg, Span::single(line, col)));
    }

    fn emit_code(&mut self, code: &str, msg: impl Into<String>) {
        let span = Span::single(self.current_line, self.current_col);
        self.errors.push(Diagnostic::error_code(code, msg, span));
    }

    pub(crate) fn emit_with_code(&mut self, code: &str, msg: impl Into<String>, span: Span) {
        self.errors.push(Diagnostic::error_code(code, msg, span));
    }

    fn push_borrow_scope(&mut self) {
        self.borrows.push(HashMap::new());
    }

    fn pop_borrow_scope(&mut self) {
        self.borrows.pop();
    }

    fn lookup_borrow(&self, name: &str) -> Option<&BorrowState> {
        for scope in self.borrows.iter().rev() {
            if let Some(state) = scope.get(name) {
                return Some(state);
            }
        }
        None
    }

    fn set_borrow(&mut self, name: &str, state: BorrowState) {
        if let Some(scope) = self.borrows.last_mut() {
            scope.insert(name.into(), state);
        }
    }

    /// Release a borrow (set back to Unborrowed) — NLL last-use release
    fn release_borrow(&mut self, name: &str) {
        if let Some(scope) = self.borrows.last_mut() {
            scope.insert(name.into(), BorrowState::Unborrowed);
        }
    }

    /// Collect all variable names used in an expression (shallow)
    fn collect_uses_in_expr(expr: &Expr, uses: &mut Vec<String>) {
        match expr {
            Expr::Ident(name) => uses.push(name.clone()),
            Expr::Unary(_, inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Binary(_, l, r) => {
                Self::collect_uses_in_expr(l, uses);
                Self::collect_uses_in_expr(r, uses);
            }
            Expr::Call(callee, args) => {
                Self::collect_uses_in_expr(callee, uses);
                for arg in args {
                    Self::collect_uses_in_expr(arg, uses);
                }
            }
            Expr::Field(obj, _) => Self::collect_uses_in_expr(obj, uses),
            Expr::TupleIndex(obj, _) => Self::collect_uses_in_expr(obj, uses),
            Expr::Index(obj, idx) => {
                Self::collect_uses_in_expr(obj, uses);
                Self::collect_uses_in_expr(idx, uses);
            }
            Expr::If { cond, then_, else_ } => {
                Self::collect_uses_in_expr(cond, uses);
                for s in then_ { Self::collect_uses_in_stmt(s, uses); }
                if let Some(e) = else_ { for s in e { Self::collect_uses_in_stmt(s, uses); } }
            }
            Expr::Tuple(elems) => { for e in elems { Self::collect_uses_in_expr(e, uses); } }
            Expr::List(elems) => { for e in elems { Self::collect_uses_in_expr(e, uses); } }
            Expr::Lambda { body, .. } => { for s in body { Self::collect_uses_in_stmt(s, uses); } }
            Expr::Match(scrutinee, arms) => {
                Self::collect_uses_in_expr(scrutinee, uses);
                for arm in arms { Self::collect_uses_in_expr(&arm.body, uses); }
            }
            Expr::Try(inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Spawn(inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Await(inner) => Self::collect_uses_in_expr(inner, uses),
            Expr::Range { start, end } => {
                Self::collect_uses_in_expr(start, uses);
                Self::collect_uses_in_expr(end, uses);
            }
            Expr::SliceExpr { target, start, end } => {
                Self::collect_uses_in_expr(target, uses);
                if let Some(s) = start { Self::collect_uses_in_expr(s, uses); }
                if let Some(e) = end { Self::collect_uses_in_expr(e, uses); }
            }
            Expr::Turbofish(_, _, args) => { for a in args { Self::collect_uses_in_expr(a, uses); } }
            Expr::Literal(_) | Expr::Old(_) | Expr::Comptime(_) | Expr::Quote(_) | Expr::QuoteInterpolate(_) | Expr::TypeInfo(_) | Expr::TypeOf(_) => {}
            Expr::Record { fields, .. } => { for f in fields { Self::collect_uses_in_expr(&f.value, uses); } }
            Expr::Comprehension { expr, iter, guard, .. } => {
                Self::collect_uses_in_expr(expr, uses);
                Self::collect_uses_in_expr(iter, uses);
                if let Some(g) = guard { Self::collect_uses_in_expr(g, uses); }
            }
        }
    }

    /// Collect all variable names used in a statement
    fn collect_uses_in_stmt(stmt: &Stmt, uses: &mut Vec<String>) {
        match stmt {
            Stmt::Expr(e) => Self::collect_uses_in_expr(e, uses),
            Stmt::Return(Some(e)) => Self::collect_uses_in_expr(e, uses),
            Stmt::Return(None) => {}
            Stmt::Let { init: Some(e), .. } => Self::collect_uses_in_expr(e, uses),
            Stmt::Let { init: None, .. } => {}
            Stmt::Assign { target, value } => {
                Self::collect_uses_in_expr(target, uses);
                Self::collect_uses_in_expr(value, uses);
            }
            Stmt::If { cond, then_, else_ } => {
                Self::collect_uses_in_expr(cond, uses);
                for s in then_ { Self::collect_uses_in_stmt(s, uses); }
                if let Some(e) = else_ { for s in e { Self::collect_uses_in_stmt(s, uses); } }
            }
            Stmt::While { cond, body } => {
                Self::collect_uses_in_expr(cond, uses);
                for s in body { Self::collect_uses_in_stmt(s, uses); }
            }
            Stmt::For { iterable, body, .. } => {
                Self::collect_uses_in_expr(iterable, uses);
                for s in body { Self::collect_uses_in_stmt(s, uses); }
            }
            Stmt::Block(block) => { for s in block { Self::collect_uses_in_stmt(s, uses); } }
            Stmt::Break(Some(e)) => Self::collect_uses_in_expr(e, uses),
            Stmt::Break(None) | Stmt::Continue => {}
            Stmt::Requires(e, _) | Stmt::Ensures(e, _) | Stmt::Drop(e) => Self::collect_uses_in_expr(e, uses),
            Stmt::SharedLet { init, .. } => Self::collect_uses_in_expr(init, uses),
            Stmt::Arena(block) | Stmt::OnFailure(block) | Stmt::Parasteps(block) | Stmt::Unsafe(block) => {
                for s in block { Self::collect_uses_in_stmt(s, uses); }
            }
            Stmt::Math(exprs) => { for e in exprs { Self::collect_uses_in_expr(e, uses); } }
            Stmt::Alloc { body, .. } => { for s in body { Self::collect_uses_in_stmt(s, uses); } }
            Stmt::MmsBlock { .. } | Stmt::Ellipsis | Stmt::Desc(_) => {}
        }
    }

    /// NLL: Release borrows at their last use within a block.
    /// Called before checking statement `current_idx`. Releases any borrow whose
    /// borrow reference variable is NOT used in the current or any later statement.
    fn release_borrows_at_last_use(&mut self, block: &[Stmt], current_idx: usize) {
        // Collect currently borrowed variables and their borrow reference names
        let borrows: Vec<(String, String)> = {
            if let Some(scope) = self.borrows.last() {
                scope.iter()
                    .filter(|(_, state)| !matches!(state, BorrowState::Unborrowed))
                    .map(|(name, _)| {
                        // Find the borrow reference variable name
                        // It's typically: let r = &x  -> borrow_ref = "r", borrowed_var = "x"
                        let borrow_ref = self.find_borrow_ref(name, block, current_idx);
                        (name.clone(), borrow_ref)
                    })
                    .collect()
            } else {
                vec![]
            }
        };

        for (borrowed_var, borrow_ref) in &borrows {
            if matches!(self.lookup_borrow(borrowed_var), Some(BorrowState::Unborrowed) | None) {
                continue;
            }

            // NLL: Release borrow if the reference variable is completely unused from now on.
            // Check: is the reference used in any statement from current_idx onward?
            let ref_used_after = block[current_idx..].iter().any(|s| {
                let mut uses = Vec::new();
                Self::collect_uses_in_stmt(s, &mut uses);
                uses.contains(borrow_ref)
            });

            // Release only if ref is not used from current point onward
            if !ref_used_after {
                self.release_borrow(borrowed_var);
            }
        }
    }

    /// Find the name of the variable that holds a borrow reference to `borrowed_var`.
    /// Scans earlier statements for `let ref_name = &borrowed_var` patterns.
    fn find_borrow_ref(&self, borrowed_var: &str, block: &[Stmt], current_idx: usize) -> String {
        for stmt in &block[..current_idx] {
            if let Stmt::Let { pat, init: Some(Expr::Unary(UnOp::Ref, inner)), .. } = stmt {
                if let Expr::Ident(name) = inner.as_ref() {
                    if name == borrowed_var {
                        if let Pattern::Variable(ref_name) = pat {
                            return ref_name.clone();
                        }
                    }
                }
            }
            if let Stmt::Let { pat, init: Some(Expr::Unary(UnOp::RefMut, inner)), .. } = stmt {
                if let Expr::Ident(name) = inner.as_ref() {
                    if name == borrowed_var {
                        if let Pattern::Variable(ref_name) = pat {
                            return ref_name.clone();
                        }
                    }
                }
            }
        }
        borrowed_var.to_string()
    }

    fn collect_decls(&mut self) {
        // Process imports: add module names to use_imports
        for import in &self.file.imports {
            if let Some(module_name) = import.path.first() {
                self.use_imports.push(module_name.clone());
            }
        }
        for item in &self.file.items {
            self.collect_item_decls(item);
        }
        // Check for type alias cycles
        self.check_alias_cycles();
    }

    /// Detect type alias cycles: type A = B; type B = A;
    fn check_alias_cycles(&mut self) {
        let alias_names: Vec<String> = self.aliases.keys().cloned().collect();
        for name in &alias_names {
            let mut visited = std::collections::HashSet::new();
            visited.insert(name.clone());
            if self.follows_alias_cycle(name, &visited) {
                self.emit_code(crate::diagnostic::codes::E0409, format!("type alias cycle detected: '{}' forms a cycle", name));
            }
        }
    }

    fn follows_alias_cycle(&self, name: &str, visited: &std::collections::HashSet<String>) -> bool {
        if let Some(Type::Name(target, _)) = self.aliases.get(name) {
            if visited.contains(target) {
                return true;
            }
            let mut new_visited = visited.clone();
            new_visited.insert(target.clone());
            return self.follows_alias_cycle(target, &new_visited);
        }
        false
    }

    fn collect_item_decls(&mut self, item: &Item) {
        match item {
            Item::Func(f) => {
                self.set_pos(f.pos.0, f.pos.1);
                let qualified_name = if self.module_path.is_empty() {
                    f.name.clone()
                } else {
                    format!("{}::{}", self.module_path.join("::"), f.name)
                };
                if self.funcs.contains_key(&qualified_name) {
                    self.emit_code(crate::diagnostic::codes::E0402, format!("duplicate function definition '{}'", qualified_name));
                    return;
                }
                let generic_names: Vec<String> = f.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(generic_names.iter().cloned());
                let params: Vec<Type> = f.params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                let ret = f
                    .ret
                    .as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                let allow_passport = f.extern_abi.is_some();
                for (i, p) in f.params.iter().enumerate() {
                    if allow_passport {
                        self.check_type_well_formed_allow_passport(&params[i], &format!("parameter '{}' of function '{}'", p.name, qualified_name));
                    } else {
                        self.check_type_well_formed(&params[i], &format!("parameter '{}' of function '{}'", p.name, qualified_name));
                    }
                }
                if allow_passport {
                    self.check_type_well_formed_allow_passport(&ret, &format!("return type of function '{}'", qualified_name));
                } else {
                    self.check_type_well_formed(&ret, &format!("return type of function '{}'", qualified_name));
                }
                self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
                self.funcs.insert(qualified_name.clone(), (params, ret));
                // Store generic parameters if present
                if !f.generics.is_empty() {
                    self.func_generics.insert(qualified_name.clone(), f.generics.clone());
                }
                // Store where clause if present
                if let Some(where_clause) = &f.where_clause {
                    self.where_clauses.insert(
                        qualified_name.clone(),
                        (where_clause.type_param.clone(), where_clause.bounds.clone()),
                    );
                }
                // Store effects if present
                if !f.effects.is_empty() {
                    self.func_effects.insert(qualified_name, f.effects.clone());
                }
            }
            Item::Type(t) => {
                if self.types.contains_key(&t.name) {
                    self.emit_code(crate::diagnostic::codes::E0402, format!("duplicate type definition '{}'", t.name));
                    return;
                }
                let generic_names: Vec<String> = t.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(generic_names.iter().cloned());
                match &t.kind {
                    TypeDefKind::Alias(ty) => {
                        let resolved = self.resolve_type(ty);
                        self.check_type_well_formed(&resolved, &format!("alias '{}'", t.name));
                        self.aliases.insert(t.name.clone(), resolved);
                    }
                    TypeDefKind::Newtype(ty) => {
                        // Store the newtype with its inner type (unresolved for now)
                        self.newtypes.insert(t.name.clone(), ty.clone());
                        // The inner type is what the constructor takes as input
                        let inner = self.resolve_type(ty);
                        self.check_type_well_formed(&inner, &format!("newtype '{}'", t.name));
                        // The return type is the newtype itself, wrapped in Type::Newtype with name
                        let self_ty = Type::Newtype(t.name.clone(), Box::new(inner.clone()));
                        self.funcs.insert(t.name.clone(), (vec![inner], self_ty));
                    }
                    TypeDefKind::Enum(variants) => {
                        let self_ty = Type::Name(t.name.clone(), vec![]);
                        for v in variants {
                            let ret = self_ty.clone();
                            let params = match &v.payload {
                                None => vec![],
                                Some(VariantPayload::Tuple(types)) => types.iter().map(|ty| self.resolve_type(ty)).collect(),
                                Some(VariantPayload::Record(fields)) => fields.iter().map(|f| self.resolve_type(&f.ty)).collect(),
                            };
                            for p in &params {
                                self.check_type_well_formed(p, &format!("variant '{}' of enum '{}'", v.name, t.name));
                            }
                            self.funcs.insert(v.name.clone(), (params, ret));
                        }
                    }
                    TypeDefKind::Record(fields) => {
                        for field in fields {
                            let field_ty = self.resolve_type(&field.ty);
                            self.check_type_well_formed(&field_ty, &format!("field '{}' of record '{}'", field.name, t.name));
                        }
                    }
                    TypeDefKind::Union(fields) => {
                        for field in fields {
                            let field_ty = self.resolve_type(&field.ty);
                            self.check_type_well_formed(&field_ty, &format!("field '{}' of union '{}'", field.name, t.name));
                        }
                    }
                }
                self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
                self.types.insert(t.name.clone(), t.clone());
                // Store generic parameters for type definitions
                if !t.generics.is_empty() {
                    self.type_generics.insert(t.name.clone(), t.generics.clone());
                }
            }
            Item::Module(m) => {
                self.module_path.push(m.name.clone());
                for inner in &m.items {
                    self.collect_item_decls(inner);
                }
                self.module_path.pop();
            }
            Item::Actor(actor) => {
                // Register actor type so it can be used as a type
                let actor_type_def = TypeDef {
                    name: actor.name.clone(),
                    commitment: actor.commitment,
                    pub_: actor.pub_,
                    kind: TypeDefKind::Record(actor.fields.iter().map(|f| Field {
                        name: f.name.clone(),
                        ty: f.ty.clone(),
                    }).collect()),
                    generics: Vec::new(),
                    derives: Vec::new(),
                    attributes: Vec::new(),
                };
                self.types.insert(actor.name.clone(), actor_type_def);

                // Collect actor methods as functions
                for method in &actor.methods {
                    if self.funcs.contains_key(&method.name) {
                        self.emit_code(crate::diagnostic::codes::E0402, format!("duplicate function definition '{}'", method.name));
                        return;
                    }
                    let generic_names: Vec<String> = method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(generic_names.iter().cloned());
                    // Add implicit self parameter as first param
                    let self_type = Type::Name(actor.name.clone(), vec![]);
                    let mut params = vec![self_type];
                    params.extend(method.params.iter().map(|p| self.resolve_type(&p.ty)));
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    for (i, p) in method.params.iter().enumerate() {
                        self.check_type_well_formed(&params[i + 1], &format!("parameter '{}' of actor method '{}'", p.name, method.name));
                    }
                    self.check_type_well_formed(&ret, &format!("return type of actor method '{}'", method.name));
                    self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
                    self.funcs.insert(method.name.clone(), (params, ret));
                }
            }
            Item::Rule(_) | Item::Desc(_) | Item::Cap(_) => {}
            Item::Trait(trait_def) => {
                let method_names: Vec<String> = trait_def.methods.iter().map(|m| m.name.clone()).collect();
                self.traits.insert(trait_def.name.clone(), method_names.clone());
                let generic_names: Vec<String> = trait_def.generics.iter().map(|g| g.name.clone()).collect();
                self.trait_generics.insert(trait_def.name.clone(), generic_names.clone());
                // Push trait generics into scope so method signatures can reference them
                self.generic_scope.extend(generic_names.iter().cloned());
                // Store trait method signatures for argument validation
                for method in &trait_def.methods {
                    let params: Vec<Type> = method.params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                    let ret = method.ret.as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    self.trait_method_sigs.insert(
                        (trait_def.name.clone(), method.name.clone()),
                        (params, ret),
                    );
                }
                self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
            }
            Item::Impl(impl_def) => {
                let method_names: Vec<String> = impl_def.methods.iter().map(|m| m.name.clone()).collect();
                self.impls.insert(
                    (impl_def.trait_name.clone(), impl_def.type_name.clone()),
                    method_names.clone(),
                );
                // Register methods available on this type via this trait
                for method_name in &method_names {
                    self.type_methods
                        .entry(impl_def.type_name.clone())
                        .or_default()
                        .push((impl_def.trait_name.clone(), method_name.clone()));
                }
                // Also register impl methods as functions with self parameter
                let impl_generic_names: Vec<String> = impl_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(impl_generic_names.iter().cloned());
                for method in &impl_def.methods {
                    let generic_names: Vec<String> = method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(generic_names.iter().cloned());
                    let mut params = vec![Type::Name(impl_def.type_name.clone(), impl_def.type_args.clone())];
                    params.extend(method.params.iter().map(|p| self.resolve_type(&p.ty)));
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    for (i, p) in method.params.iter().enumerate() {
                        self.check_type_well_formed(&params[i + 1], &format!("parameter '{}' of impl method '{}'", p.name, method.name));
                    }
                    self.check_type_well_formed(&ret, &format!("return type of impl method '{}'", method.name));
                    self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
                    let key = format!("{}_{}", impl_def.type_name, method.name);
                    self.funcs.insert(key, (params, ret));
                }
                self.generic_scope.truncate(self.generic_scope.len() - impl_generic_names.len());
            }
            Item::ExternBlock(block) => {
                // Register extern functions for type checking
                for func in &block.funcs {
                    for param in &func.params {
                        let resolved = self.resolve_type(&param.ty);
                        if !self.is_valid_extern_type(&resolved, false) {
                            let type_str = fmt_type(&resolved);
                            let help = if type_str.contains("List") || type_str.starts_with('[') {
                                format!("type '{}' is a Mimi list/array and cannot cross the C ABI boundary directly. \
                                    Use a pointer (*T / *mut T) to pass array data, or serialize to JSON via the builtin JSON module.", type_str)
                            } else if type_str.contains("Option") || type_str.contains("Result") {
                                format!("type '{}' is an algebraic data type and cannot cross the C ABI boundary. \
                                    Use a plain type or a pointer (*T).", type_str)
                            } else {
                                format!("type '{}' is not allowed across the C ABI boundary. \
                                    Use scalar types (i32, i64, f64, bool, string), or *T, *mut T, c_shared T, c_borrow T, c_borrow_mut T, cap, #[repr(C)] records.", type_str)
                            };
                            self.emit_code(crate::diagnostic::codes::E0231, format!(
                                "extern function parameter '{}' has type '{}', which is not allowed to cross the C ABI boundary. {}",
                                param.name, type_str, help
                            ));
                        }
                    }
                    let params: Vec<Type> = func.params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                    let ret = func.ret.as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    self.funcs.insert(func.name.clone(), (params, ret));
                }
            }
        }
    }

    fn resolve_type(&self, ty: &Type) -> Type {
        match ty {
            Type::Name(name, args) => {
                if let Some(aliased) = self.aliases.get(name) {
                    if let Some(generics) = self.type_generics.get(name) {
                        if !args.is_empty() && args.len() == generics.len() {
                            let type_map: HashMap<String, Type> = generics.iter()
                                .zip(args.iter())
                                .map(|(g, a)| (g.name.clone(), a.clone()))
                                .collect();
                            return subst_type_params(aliased, generics, &type_map);
                        }
                    }
                    aliased.clone()
                } else if let Some(inner_ty) = self.newtypes.get(name) {
                    // This is a newtype - wrap the resolved inner type in Type::Newtype with name
                    Type::Newtype(name.clone(), Box::new(self.resolve_type(inner_ty)))
                } else {
                    Type::Name(name.clone(), args.clone())
                }
            }
            Type::Ref(_, inner) => Type::Ref(None, Box::new(self.resolve_type(inner))),
            Type::RefMut(_, inner) => Type::RefMut(None, Box::new(self.resolve_type(inner))),
            Type::Option(inner) => Type::Option(Box::new(self.resolve_type(inner))),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.resolve_type(ok)),
                Box::new(self.resolve_type(err)),
            ),
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.resolve_type(e)).collect()),
            Type::Func(args, ret) => Type::Func(
                args.iter().map(|a| self.resolve_type(a)).collect(),
                Box::new(self.resolve_type(ret)),
            ),
            Type::ExternFunc(args, ret) => Type::ExternFunc(
                args.iter().map(|a| self.resolve_type(a)).collect(),
                Box::new(self.resolve_type(ret)),
            ),
            Type::Cap(_) | Type::Shared(_) | Type::LocalShared(_) | Type::Weak(_) | Type::WeakLocal(_)
                | Type::CShared(_) | Type::CBorrow(_) | Type::CBorrowMut(_)
                | Type::RawPtr(_) | Type::RawPtrMut(_) | Type::RawString | Type::Allocator => ty.clone(),
            Type::CBuffer(inner) => Type::CBuffer(Box::new(self.resolve_type(inner))),
            Type::Newtype(name, inner) => Type::Newtype(name.clone(), Box::new(self.resolve_type(inner))),
            Type::Array(inner, size) => Type::Array(Box::new(self.resolve_type(inner)), *size),
            Type::Slice(inner) => Type::Slice(Box::new(self.resolve_type(inner))),
            Type::Nothing => Type::Nothing,
            Type::Infer => Type::Infer,
        Type::ImplTrait(traits) => Type::ImplTrait(traits.clone()),
        Type::DynTrait(traits) => Type::DynTrait(traits.clone()),
        }
    }

    /// Check whether a type is allowed to cross the C ABI boundary in an
    /// extern function signature.
    fn is_valid_extern_type(&self, ty: &Type, _in_pointer: bool) -> bool {
        match ty {
            // Scalars and #[repr(C)] user types (Enum, Record, Union)
            Type::Name(name, _) => {
                matches!(name.as_str(), "i32" | "i64" | "f64" | "bool" | "string" | "unit")
                || (self.types.get(name).map(|t| t.attributes.contains(&TypeAttribute::ReprC)).unwrap_or(false)
                    && matches!(self.types.get(name).map(|t| &t.kind),
                        Some(TypeDefKind::Enum(_)) | Some(TypeDefKind::Record(_)) | Some(TypeDefKind::Union(_))))
            }
            // Capabilities
            Type::Cap(_) => true,
            // Raw pointers and FFI passport types
            Type::RawPtr(_) | Type::RawPtrMut(_) | Type::CShared(_) | Type::CBorrow(_) | Type::CBorrowMut(_) => true,
            // Raw string ownership transfer
            Type::RawString => true,
            // C function pointers
            Type::ExternFunc(_, _) => true,
            // C buffer with automatic memory management
            Type::CBuffer(_) => true,
            // References are not allowed directly; must use c_borrow / c_borrow_mut
            Type::Ref(_, _) | Type::RefMut(_, _) => false,
            // Shared ownership is not allowed directly; must use c_shared
            Type::Shared(_) | Type::LocalShared(_) | Type::Weak(_) | Type::WeakLocal(_) => false,
            // Composite Mimi types
            // Tuple is allowed — serialized as JSON over FFI boundary
            Type::Tuple(_) => true,
            Type::Option(_) | Type::Result(_, _) => false,
            Type::Array(_, _) | Type::Slice(_) => false,
            // G1b: Accept closures (Type::Func) as extern callback params
            Type::Func(_, _) => true,
            Type::Newtype(name, inner) => {
                if let Some(tdef) = self.types.get(name) {
                    if tdef.attributes.contains(&TypeAttribute::ReprC) {
                        return self.is_valid_extern_type(inner, _in_pointer);
                    }
                }
                false
            }
            Type::ImplTrait(_) => false,
            Type::DynTrait(_) => false,
            Type::Nothing | Type::Allocator | Type::Infer => false,
        }
    }

    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Func(f) => {
                self.set_pos(f.pos.0, f.pos.1);
                // Strict mode: check commitment locks
                if self.strict {
                    self.check_commitment_locks(f.name.as_str(), f.commitment, &f.body);
                }
                self.check_func(f)
            }
            Item::Module(m) => {
                for inner in &m.items {
                    self.check_item(inner);
                }
            }
            Item::Actor(actor) => {
                // Check actor fields
                for field in &actor.fields {
                    let field_ty = self.resolve_type(&field.ty);
                    // Validate field type is well-formed
                    self.check_type_well_formed(&field_ty, &format!("actor field '{}'", field.name));
                    // Check field initialization if present
                    if let Some(init) = &field.init {
                        let init_ty = self.infer_expr(init, &mut vec![HashMap::new()]);
                        if !same_type(&field_ty, &init_ty) {
                            self.emit_code(crate::diagnostic::codes::E0209, format!(
                                "actor field '{}' initializer type {} does not match field type {}",
                                field.name,
                                fmt_type(&init_ty),
                                fmt_type(&field_ty)
                            ));
                        }
                    }
                }
                // Check actor methods
                for method in &actor.methods {
                    self.set_pos(method.pos.0, method.pos.1);
                    // Add implicit self parameter to scope for actor methods
                    let self_ty = Type::Name(actor.name.clone(), vec![]);
                    let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
                    scopes[0].insert("self".to_string(), self_ty);
                    // Add other params
                    for p in &method.params {
                        let ty = self.resolve_type(&p.ty);
                        scopes[0].insert(p.name.clone(), ty);
                    }
                    // Check block with self in scope
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    self.var_scopes.push(HashMap::new());
                    self.cap_vars.push(HashMap::new());
                    self.check_block(&method.body, &ret, &mut scopes);
                    self.check_unconsumed_caps();
                    self.cap_vars.pop();
                    self.var_scopes.pop();
                }
            }
            Item::Type(_) | Item::Cap(_) => {}
            Item::Rule(_) | Item::Desc(_) => {}
            Item::Trait(trait_def) => {
                // Check that all trait method types are well-formed
                let generic_names: Vec<String> = trait_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(generic_names.iter().cloned());
                for method in &trait_def.methods {
                    let method_generic_names: Vec<String> = method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(method_generic_names.iter().cloned());
                    for param in &method.params {
                        let resolved = self.resolve_type(&param.ty);
                        self.check_type_well_formed(&resolved, &format!("trait '{}' method '{}'", trait_def.name, method.name));
                    }
                    if let Some(ret) = &method.ret {
                        let resolved = self.resolve_type(ret);
                        self.check_type_well_formed(&resolved, &format!("trait '{}' method '{}' return", trait_def.name, method.name));
                    }
                    self.generic_scope.truncate(self.generic_scope.len() - method_generic_names.len());
                }
                self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
            }
            Item::Impl(impl_def) => {
                // Check that the trait exists
                if !self.traits.contains_key(&impl_def.trait_name) {
                    self.emit_code(crate::diagnostic::codes::E0406, format!("undefined trait '{}'", impl_def.trait_name));
                }
                // Check that the type exists
                if !self.types.contains_key(&impl_def.type_name) && !Self::is_builtin_type(&impl_def.type_name) {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0407,
                            format!("undefined type '{}'", impl_def.type_name),
                            Span::single(self.current_line, self.current_col),
                        ).with_help("types must be defined before use — check the type name spelling or add a 'type' declaration")
                    );
                }
                // Check that all required trait methods are implemented
                if let Some(required_methods) = self.traits.get(&impl_def.trait_name).cloned() {
                    let implemented: Vec<String> = impl_def.methods.iter().map(|m| m.name.clone()).collect();
                    for required in &required_methods {
                        if !implemented.contains(required) {
                            self.emit_code(crate::diagnostic::codes::E0252, format!(
                                "missing method '{}' in impl of trait '{}' for '{}'",
                                required, impl_def.trait_name, impl_def.type_name
                            ));
                        }
                    }
                }
                // Check impl method bodies with self bound to the implementing type
                let impl_generic_names: Vec<String> = impl_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(impl_generic_names.iter().cloned());
                for method in &impl_def.methods {
                    self.set_pos(method.pos.0, method.pos.1);
                    let method_generic_names: Vec<String> = method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(method_generic_names.iter().cloned());
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
                    // Bind self with the implementing type
                    scopes[0].insert("self".to_string(), Type::Name(impl_def.type_name.clone(), impl_def.type_args.clone()));
                    for p in &method.params {
                        let ty = self.resolve_type(&p.ty);
                        scopes[0].insert(p.name.clone(), ty);
                    }
                    self.var_scopes.push(HashMap::new());
                    self.cap_vars.push(HashMap::new());
                    self.check_block(&method.body, &ret, &mut scopes);
                    self.check_unconsumed_caps();
                    self.var_scopes.pop();
                    self.cap_vars.pop();
                    self.generic_scope.truncate(self.generic_scope.len() - method_generic_names.len());
                }
                self.generic_scope.truncate(self.generic_scope.len() - impl_generic_names.len());
            }
            Item::ExternBlock(_) => {
                // Extern blocks are collected but not type-checked in v1.1
            }
        }
    }

    fn is_builtin_type(name: &str) -> bool {
        Self::builtin_type_names().contains(&name.to_string())
    }

    fn builtin_type_names() -> Vec<String> {
        vec![
            "i32".into(), "i64".into(), "f64".into(), "bool".into(),
            "string".into(), "unit".into(), "List".into(), "Future".into(),
            "Result".into(), "Option".into(),
        ]
    }

    fn check_type_well_formed(&mut self, ty: &Type, context: &str) {
        self.check_type_well_formed_inner(ty, context, false);
    }

    #[allow(dead_code)]
    fn check_type_well_formed_allow_passport(&mut self, ty: &Type, context: &str) {
        self.check_type_well_formed_inner(ty, context, true);
    }

    fn check_type_well_formed_inner(&mut self, ty: &Type, context: &str, allow_passport: bool) {
        if !allow_passport && Self::type_contains_passport(ty) {
            self.emit_code(crate::diagnostic::codes::E0231, format!(
                "FFI passport type '{}' is not allowed in {}",
                fmt_type(ty), context
            ));
            return;
        }
        match ty {
            Type::Name(name, args) => {
                if !Self::is_builtin_type(name)
                    && !self.types.contains_key(name)
                    && !self.generic_scope.contains(name)
                {
                    let mut candidates: Vec<String> = self.types.keys().cloned().collect();
                    candidates.extend(self.generic_scope.clone());
                    candidates.extend(Self::builtin_type_names());
                    let suggestion = suggest_name(name, &candidates, 3);
                    let help = if let Some(suggested) = suggestion {
                        format!("type '{}' not found — did you mean '{}'?", name, suggested)
                    } else {
                        "check the type name spelling or add a 'type' declaration".to_string()
                    };
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0407,
                            format!("unknown type '{}' in {}", name, context),
                            Span::single(self.current_line, self.current_col),
                        ).with_help(help)
                    );
                }
                for arg in args {
                    self.check_type_well_formed_inner(arg, context, allow_passport);
                }
            }
            Type::Ref(_, inner) | Type::RefMut(_, inner) | Type::Option(inner)
                | Type::Shared(inner) | Type::LocalShared(inner) | Type::Weak(inner)
                | Type::WeakLocal(inner)
                | Type::RawPtr(inner) | Type::RawPtrMut(inner)
                | Type::CShared(inner) | Type::CBorrow(inner) | Type::CBorrowMut(inner) => {
                self.check_type_well_formed_inner(inner, context, allow_passport);
            }
            Type::RawString => { /* no inner type to check */ }
            Type::Result(ok, err) => {
                self.check_type_well_formed_inner(ok, context, allow_passport);
                self.check_type_well_formed_inner(err, context, allow_passport);
            }
            Type::Tuple(elems) => {
                for elem in elems {
                    self.check_type_well_formed_inner(elem, context, allow_passport);
                }
            }
            Type::Func(args, ret) => {
                for arg in args {
                    self.check_type_well_formed_inner(arg, context, allow_passport);
                }
                self.check_type_well_formed_inner(ret, context, allow_passport);
            }
            Type::ExternFunc(args, ret) => {
                for arg in args {
                    self.check_type_well_formed_inner(arg, context, allow_passport);
                }
                self.check_type_well_formed_inner(ret, context, allow_passport);
            }
            Type::CBuffer(inner) => {
                self.check_type_well_formed_inner(inner, context, allow_passport);
            }
            Type::Newtype(name, inner) => {
                if !self.types.contains_key(name) && !self.newtypes.contains_key(name) {
                    self.emit_code(crate::diagnostic::codes::E0407, format!("unknown newtype '{}' in {}", name, context));
                }
                self.check_type_well_formed_inner(inner, context, allow_passport);
            }
            Type::Cap(_) | Type::Nothing | Type::Allocator | Type::Infer => {}
            Type::Array(inner, _) | Type::Slice(inner) => {
                self.check_type_well_formed_inner(inner, context, allow_passport);
            }
            Type::ImplTrait(_traits) => {
            }
            Type::DynTrait(traits) => {
                for trait_name in traits {
                    if !self.traits.contains_key(trait_name) {
                        self.emit_code(crate::diagnostic::codes::E0406, format!("unknown trait '{}' in dyn Trait in {}", trait_name, context));
                    }
                }
            }
        }
    }

    /// Returns true if the type (or any type nested inside it) is one of the
    /// FFI boundary passport types.
    fn type_contains_passport(ty: &Type) -> bool {
        match ty {
            Type::RawPtr(_) | Type::RawPtrMut(_)
                | Type::CShared(_) | Type::CBorrow(_) | Type::CBorrowMut(_)
                | Type::RawString => true,
            Type::Name(_, args) => args.iter().any(Self::type_contains_passport),
            Type::Ref(_, inner) | Type::RefMut(_, inner) | Type::Option(inner)
                | Type::Shared(inner) | Type::LocalShared(inner) | Type::Weak(inner)
                | Type::WeakLocal(inner)
                | Type::Array(inner, _) | Type::Slice(inner) => Self::type_contains_passport(inner),
            Type::Result(ok, err) => Self::type_contains_passport(ok) || Self::type_contains_passport(err),
            Type::Tuple(elems) => elems.iter().any(Self::type_contains_passport),
            Type::Func(args, ret) => args.iter().any(Self::type_contains_passport) || Self::type_contains_passport(ret),
            Type::ExternFunc(args, ret) => args.iter().any(Self::type_contains_passport) || Self::type_contains_passport(ret),
            Type::CBuffer(inner) => Self::type_contains_passport(inner),
            Type::Newtype(_, inner) => Self::type_contains_passport(inner),
            Type::Cap(_) | Type::Nothing | Type::Allocator | Type::Infer => false,
            Type::ImplTrait(_) => false,
            Type::DynTrait(_) => false,
        }
    }

    /// Check if a type implements a trait
    fn type_implements_trait(&self, ty: &Type, trait_name: &str) -> bool {
        match ty {
            Type::Name(type_name, _) => {
                self.impls.contains_key(&(trait_name.to_string(), type_name.clone()))
            }
            _ => false,
        }
    }

    fn check_func(&mut self, func: &FuncDef) {
        let ret = func
            .ret
            .as_ref()
            .map(|t| self.resolve_type(t))
            .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
        let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
        // Push function-level variable scope for shadowing detection
        self.var_scopes.push(HashMap::new());
        // Push cap scope for function body
        self.cap_vars.push(HashMap::new());
        for p in &func.params {
            let ty = self.resolve_type(&p.ty);
            // If param is a cap type, track it
            if matches!(&ty, Type::Cap(_)) {
                if let Some(s) = self.cap_vars.last_mut() {
                    s.insert(p.name.clone(), false);
                }
            }
            scopes[0].insert(p.name.clone(), ty);
        }
        // Comptime functions: type-check body but mark as compile-time evaluable
        if func.is_comptime {
            // Comptime functions can only use pure expressions (no side effects)
            // For now, just type-check the body normally
        }
        // Check all-return-paths requirement
        if !matches!(&ret, Type::Name(n, _) if n == "unit") && !self.block_returns_on_all_paths(&func.body) {
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0255,
                    format!("function '{}' does not return on all paths (missing return in some branches)", func.name),
                    Span::single(self.current_line, self.current_col),
                ).with_help("add a return statement or make the last expression return the appropriate type")
            );
        }
        self.check_block(&func.body, &ret, &mut scopes);
        // Check for unconsumed caps before popping
        self.check_unconsumed_caps();
        self.var_scopes.pop();
        self.cap_vars.pop();
    }

    /// Check if a block returns on all paths
    fn block_returns_on_all_paths(&self, block: &Block) -> bool {
        if block.is_empty() {
            return false;
        }
        // Check if the last statement is an implicit return (expression statement)
        if let Some(last) = block.last() {
            match last {
                Stmt::Return(_) => return true,
                Stmt::Expr(_) => return true, // implicit return via last expression
                Stmt::If { then_, else_, .. } => {
                    let then_returns = self.block_returns_on_all_paths(then_);
                    let else_returns = else_.as_ref()
                        .map(|e| self.block_returns_on_all_paths(e))
                        .unwrap_or(false);
                    if then_returns && else_returns {
                        return true;
                    }
                }
                Stmt::Block(inner) => {
                    if self.block_returns_on_all_paths(inner) {
                        return true;
                    }
                }
                Stmt::Arena(inner) => {
                    if self.block_returns_on_all_paths(inner) {
                        return true;
                    }
                }
                Stmt::Alloc { kind: _, body } => {
                    if self.block_returns_on_all_paths(body) {
                        return true;
                    }
                }
                Stmt::Expr(Expr::Match(_, arms)) => {
                    return arms.iter().all(|arm| {
                        let block = vec![Stmt::Expr(arm.body.clone())];
                        self.block_returns_on_all_paths(&block)
                    });
                }
                _ => {}
            }
        }
        false
    }

    fn check_unconsumed_caps(&mut self) {
        if let Some(scope) = self.cap_vars.last() {
            let unconsumed: Vec<String> = scope.iter()
                .filter(|(_, consumed)| !*consumed)
                .map(|(name, _)| name.clone())
                .collect();
            for name in unconsumed {
                self.emit_code(crate::diagnostic::codes::E0256, format!(
                    "linear capability '{}' must be consumed (via drop) before end of scope",
                    name
                ));
            }
        }
    }

    /// Check commitment locks in strict mode
    fn check_commitment_locks(&mut self, name: &str, commitment: Commitment, body: &Block) {
        match commitment {
            Commitment::StrongLocked | Commitment::StrongLockedQuestion | Commitment::StrongLockedQuestionQuestion => {
                // $$ locked: any modification to the function body is an error
                // Check for mms blocks that contain modified contracts
                for stmt in body {
                    if let Stmt::MmsBlock { content: text, .. } = stmt {
                        if text.contains("requires:") || text.contains("ensures:") || text.contains("math:") {
                            // In strict mode, $$ locked functions should not have their contracts changed
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0501,
                                    format!("strict mode: function '{}' is $$ locked - contract modifications not allowed", name),
                                    Span::single(self.current_line, self.current_col),
                                ).with_help("remove $$ suffix to allow modification, or use $$? for AI-reviewable lock")
                            );
                        }
                    }
                }
            }
            Commitment::Locked | Commitment::LockedQuestion | Commitment::LockedQuestionQuestion => {
                // $ locked: warn about contract modifications
                for stmt in body {
                    if let Stmt::MmsBlock { content: text, .. } = stmt {
                        if text.contains("requires:") || text.contains("ensures:") || text.contains("math:") {
                            self.errors.push(
                                Diagnostic::warning_code(
                                    crate::diagnostic::codes::E0600,
                                    format!("strict mode: function '{}' is $ locked - contract modifications discouraged", name),
                                    Span::single(self.current_line, self.current_col),
                                ).with_help("remove $ suffix to allow modification")
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn check_pattern(
        &mut self,
        pat: &Pattern,
        subject: &Type,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) {
        match pat {
            Pattern::Wildcard => {}
            Pattern::Variable(name) => {
                // If the name matches an enum variant of the subject type,
                // treat it as a constructor match (no variable binding).
                let is_constructor = match subject {
                    Type::Result(_, _) => name == "Ok" || name == "Err",
                    Type::Option(_) => name == "Some" || name == "None",
                    Type::Name(tn, _) => self.types.get(tn)
                        .and_then(|t| match &t.kind { TypeDefKind::Enum(vs) => Some(vs), _ => None })
                        .map(|vs| vs.iter().any(|v| v.name == *name))
                        .unwrap_or(false),
                    _ => false,
                };
                if !is_constructor {
                    if let Some(s) = scopes.last_mut() {
                        s.insert(name.clone(), subject.clone());
                    }
                }
            }
            Pattern::Literal(l) => {
                let lit_ty = match l {
                    Lit::Int(_) => Type::Name("i32".into(), vec![]),
                    Lit::Float(_) => Type::Name("f64".into(), vec![]),
                    Lit::Bool(_) => Type::Name("bool".into(), vec![]),
                    Lit::String(_) => Type::Name("string".into(), vec![]),
                    Lit::FString(_) => Type::Name("string".into(), vec![]),
                    Lit::Unit => Type::Name("unit".into(), vec![]),
                };
                if !same_type(subject, &lit_ty) {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0225,
                            format!(
                                "pattern literal type {} does not match subject {}",
                                fmt_type(&lit_ty),
                                fmt_type(subject)
                            ),
                            Span::single(self.current_line, self.current_col),
                        ).with_help(format!("change the pattern to match type {}", fmt_type(subject)))
                    );
                }
            }
            Pattern::Constructor(name, pats) => {
                // Handle built-in Result<T,E> constructors Ok/Err (only for Type::Result subjects)
                if (name == "Ok" || name == "Err") && matches!(subject, Type::Result(_, _)) {
                    if let Type::Result(ok_ty, err_ty) = subject {
                        let expected_ty = if name == "Ok" { ok_ty } else { err_ty };
                        if pats.len() != 1 {
                            self.emit_code(crate::diagnostic::codes::E0228, format!("'{}' expects 1 argument, got {}", name, pats.len()));
                        } else {
                            self.check_pattern(&pats[0], expected_ty, scopes);
                        }
                    }
                    return;
                }
                // Handle built-in Option<T> constructors (only for Type::Option subjects)
                if name == "Some" && matches!(subject, Type::Option(_)) {
                    if let Type::Option(inner) = subject {
                        if pats.len() != 1 {
                            self.emit_code(crate::diagnostic::codes::E0228, format!("'Some' expects 1 argument, got {}", pats.len()));
                        } else {
                            self.check_pattern(&pats[0], inner, scopes);
                        }
                    }
                    return;
                }
                if name == "None" && matches!(subject, Type::Option(_)) {
                    if !pats.is_empty() {
                        self.emit_code(crate::diagnostic::codes::E0227, "'None' expects no arguments".to_string());
                    }
                    return;
                }
                let def = self.types.values().find(|t| {
                    match &t.kind {
                        TypeDefKind::Enum(variants) => variants.iter().any(|v| v.name == *name),
                        TypeDefKind::Newtype(_) => t.name == *name,
                        _ => false,
                    }
                });
                match def {
                    Some(tdef) => {
                        match &tdef.kind {
                            TypeDefKind::Enum(variants) => {
                                if let Some(variant) = variants.iter().find(|v| v.name == *name) {
                                    match &variant.payload {
                                        None => {
                                            if !pats.is_empty() {
                                                self.emit_code(crate::diagnostic::codes::E0227, format!(
                                                    "variant '{}' takes no arguments",
                                                    name
                                                ));
                                            }
                                        }
                                        Some(VariantPayload::Tuple(types)) => {
                                            let types: Vec<Type> = types.clone();
                                            if pats.len() != types.len() {
                                                self.emit_code(crate::diagnostic::codes::E0228, format!(
                                                    "variant '{}' expects {} arguments, got {}",
                                                    name,
                                                    types.len(),
                                                    pats.len()
                                                ));
                                            } else {
                                                for (p, t) in pats.iter().zip(types.iter()) {
                                                    self.check_pattern(p, &self.resolve_type(t), scopes);
                                                }
                                            }
                                        }
                                        Some(VariantPayload::Record(fields)) => {
                                            if pats.len() != fields.len() {
                                                self.emit_code(crate::diagnostic::codes::E0228, format!(
                                                    "variant '{}' record expects {} fields, got {}",
                                                    name,
                                                    fields.len(),
                                                    pats.len()
                                                ));
                                            } else {
                                                let resolved: Vec<Type> = fields.iter().map(|f| self.resolve_type(&f.ty)).collect();
                                                for (p, t) in pats.iter().zip(resolved.iter()) {
                                                    self.check_pattern(p, t, scopes);
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    self.emit_code(crate::diagnostic::codes::E0226, format!("variant '{}' not found in type '{}'", name, tdef.name));
                                }
                            }
                            TypeDefKind::Newtype(inner) => {
                                if pats.len() != 1 {
                                    self.emit_code(crate::diagnostic::codes::E0228, format!(
                                        "newtype '{}' pattern expects exactly one argument",
                                        name
                                    ));
                                } else {
                                    self.check_pattern(&pats[0], &self.resolve_type(inner), scopes);
                                }
                            }
                            _ => {
                                self.emit_code(crate::diagnostic::codes::E0226, format!("'{}' is not an enum variant", name));
                            }
                        }
                    }
                    None => {
                        let mut constructors: Vec<String> = Vec::new();
                        for tdef in self.types.values() {
                            match &tdef.kind {
                                TypeDefKind::Enum(variants) => {
                                    constructors.extend(variants.iter().map(|v| v.name.clone()));
                                }
                                TypeDefKind::Newtype(_) => {
                                    constructors.push(tdef.name.clone());
                                }
                                _ => {}
                            }
                        }
                        let suggestion = suggest_name(name, &constructors, 3);
                        let msg = if let Some(s) = suggestion {
                            format!("undefined constructor '{}' — did you mean '{}'?", name, s)
                        } else {
                            format!("undefined constructor '{}'", name)
                        };
                        self.emit_code(crate::diagnostic::codes::E0226, msg);
                    }
                }
            }
            Pattern::Tuple(pats) => {
                match subject {
                    Type::Tuple(types) => {
                        if pats.len() != types.len() {
                            self.emit_code(crate::diagnostic::codes::E0251, format!(
                                "tuple pattern expects {} elements, found {}",
                                types.len(),
                                pats.len()
                            ));
                        } else {
                            for (p, t) in pats.iter().zip(types.iter()) {
                                self.check_pattern(p, t, scopes);
                            }
                        }
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0251, format!(
                            "cannot match tuple pattern against non-tuple type {}",
                            fmt_type(subject)
                        ));
                    }
                }
            }
            Pattern::Array(pats) => {
                match subject {
                    Type::Array(inner, size) => {
                        if pats.len() != *size {
                            self.emit_code(crate::diagnostic::codes::E0251, format!(
                                "array pattern expects {} elements, found {}",
                                size,
                                pats.len()
                            ));
                        } else {
                            for p in pats {
                                self.check_pattern(p, inner, scopes);
                            }
                        }
                    }
                    Type::Name(n, _) if n == "List" => {
                        // List pattern: check each element against the element type
                        // For now, just check against the inner type if available
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0251, format!(
                            "cannot match array pattern against non-array type {}",
                            fmt_type(subject)
                        ));
                    }
                }
            }
            Pattern::Slice(pats, rest) => {
                match subject {
                    Type::Array(inner, _) | Type::Slice(inner) => {
                        if !pats.is_empty() {
                            for p in pats {
                                self.check_pattern(p, inner, scopes);
                            }
                        }
                        if let Some(rest_pat) = rest {
                            // Rest pattern binds to a List of the element type
                            let list_ty = Type::Name("List".into(), vec![inner.as_ref().clone()]);
                            self.check_pattern(rest_pat, &list_ty, scopes);
                        }
                    }
                    _ => {
                        self.emit_code(crate::diagnostic::codes::E0251, format!(
                            "cannot match slice pattern against non-slice type {}",
                            fmt_type(subject)
                        ));
                    }
                }
            }
        }
    }

    /// Check if an effect is available in the current scope
    fn has_effect(&self, effect: &str) -> bool {
        for scope in self.available_effects.iter().rev() {
            if scope.contains_key(effect) {
                return true;
            }
        }
        false
    }

    /// Check if a type uses a type parameter
    fn type_uses_type_param(&self, ty: &Type, type_param: &str) -> bool {
        match ty {
            Type::Name(name, _) => name == type_param,
            Type::Ref(_, inner) | Type::RefMut(_, inner) | Type::Option(inner) | Type::Shared(inner) | Type::LocalShared(inner) | Type::Weak(inner) | Type::WeakLocal(inner) => {
                self.type_uses_type_param(inner, type_param)
            }
            Type::Result(ok, err) => {
                self.type_uses_type_param(ok, type_param) || self.type_uses_type_param(err, type_param)
            }
            Type::Tuple(elems) => {
                elems.iter().any(|e| self.type_uses_type_param(e, type_param))
            }
            Type::Func(args, ret) => {
                args.iter().any(|a| self.type_uses_type_param(a, type_param)) || self.type_uses_type_param(ret, type_param)
            }
            Type::Newtype(_, inner) => self.type_uses_type_param(inner, type_param),
            _ => false,
        }
    }

    /// Check if a type variable name occurs within a type (occurs check).
    /// Prevents infinite types like `T = List<T>`.
    fn occurs_check(name: &str, ty: &Type) -> bool {
        match ty {
            Type::Name(n, args) => n == name || args.iter().any(|a| Self::occurs_check(name, a)),
            Type::Ref(_, inner) | Type::RefMut(_, inner) => Self::occurs_check(name, inner),
            Type::Option(inner) => Self::occurs_check(name, inner),
            Type::Result(ok, err) => Self::occurs_check(name, ok) || Self::occurs_check(name, err),
            Type::Tuple(elems) => elems.iter().any(|e| Self::occurs_check(name, e)),
            Type::Func(args, ret) => args.iter().any(|a| Self::occurs_check(name, a)) || Self::occurs_check(name, ret),
            Type::Shared(inner) | Type::LocalShared(inner) | Type::Weak(inner) | Type::WeakLocal(inner) => Self::occurs_check(name, inner),
            Type::Newtype(_, inner) => Self::occurs_check(name, inner),
            Type::Array(inner, _) | Type::Slice(inner) => Self::occurs_check(name, inner),
            Type::ExternFunc(args, ret) => args.iter().any(|a| Self::occurs_check(name, a)) || Self::occurs_check(name, ret),
            Type::CBuffer(inner) | Type::RawPtr(inner) | Type::RawPtrMut(inner) | Type::CShared(inner) | Type::CBorrow(inner) | Type::CBorrowMut(inner) => Self::occurs_check(name, inner),
            _ => false,
        }
    }

    /// Infer type parameter bindings from a parameter type and actual argument type
    fn infer_type_params(
        &self,
        param: &Type,
        actual: &Type,
        generics: &[GenericParam],
        type_map: &mut HashMap<String, Type>,
    ) {
        match param {
            Type::Name(name, _) if is_type_param(name, generics) => {
                if !Self::occurs_check(name, actual) {
                    type_map.entry(name.clone()).or_insert_with(|| actual.clone());
                }
            }
            Type::Name(name, p_args) => {
                if is_type_param(name, generics) {
                    if !Self::occurs_check(name, actual) {
                        type_map.entry(name.clone()).or_insert_with(|| actual.clone());
                    }
                } else if !p_args.is_empty() {
                    if let Type::Name(_, a_args) = actual {
                        if p_args.len() == a_args.len() {
                            for (pa, aa) in p_args.iter().zip(a_args.iter()) {
                                self.infer_type_params(pa, aa, generics, type_map);
                            }
                        }
                    }
                }
            }
            Type::Option(inner) => {
                if let Type::Option(a_inner) = actual {
                    self.infer_type_params(inner, a_inner, generics, type_map);
                }
            }
            Type::Result(p_ok, p_err) => {
                if let Type::Result(a_ok, a_err) = actual {
                    self.infer_type_params(p_ok, a_ok, generics, type_map);
                    self.infer_type_params(p_err, a_err, generics, type_map);
                }
            }
            Type::Tuple(p_elems) => {
                if let Type::Tuple(a_elems) = actual {
                    for (pe, ae) in p_elems.iter().zip(a_elems.iter()) {
                        self.infer_type_params(pe, ae, generics, type_map);
                    }
                }
            }
            _ => {}
        }
    }

    fn lookup_var(&mut self, name: &str, scopes: &mut [HashMap<String, Type>]) -> Type {
        for scope in scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return t.clone();
            }
        }
        // Check if it's a module-qualified name via use imports
        for module in &self.use_imports.clone() {
            let qualified = format!("{}::{}", module, name);
            if let Some((params, ret)) = self.funcs.get(&qualified) {
                return Type::Func(params.clone(), Box::new(ret.clone()));
            }
        }
        // Check if it's a zero-argument constructor (enum variant without payload)
        if let Some((params, ret)) = self.funcs.get(name) {
            if params.is_empty() {
                return ret.clone();
            }
        }
        // Check if it's a type name (actor/record or enum)
        if let Some(tdef) = self.types.get(name) {
            if matches!(tdef.kind, TypeDefKind::Record(_) | TypeDefKind::Enum(_) | TypeDefKind::Union(_)) {
                // This is a type name - return it as a type
                return Type::Name(name.into(), vec![]);
            }
        }
        // Collect all known names for "did you mean?" suggestions
        let mut candidates: Vec<String> = Vec::new();
        for scope in scopes.iter().rev() {
            candidates.extend(scope.keys().cloned());
        }
        candidates.extend(self.funcs.keys().cloned());
        candidates.extend(self.types.keys().cloned());

        let suggestion = suggest_name(name, &candidates, 3);
        if let Some(suggested) = suggestion {
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0400,
                    format!("undefined variable '{}'", name),
                    Span::single(self.current_line, self.current_col),
                ).with_help(format!("did you mean '{}'?", suggested))
            );
        } else {
            self.emit_code(crate::diagnostic::codes::E0400, format!("undefined variable '{}'", name));
        }
        Type::Name("unknown".into(), vec![])
    }

    /// Get all variant names for an enum type
    fn get_enum_variants(&self, ty: &Type) -> Vec<String> {
        match ty {
            Type::Result(_, _) => {
                vec!["Ok".into(), "Err".into()]
            }
            Type::Option(_) => {
                vec!["Some".into(), "None".into()]
            }
            Type::Name(name, _) => {
                if name == "bool" {
                    // Built-in bool: pretend it has true/false variants
                    vec!["true".into(), "false".into()]
                } else if let Some(tdef) = self.types.get(name) {
                    match &tdef.kind {
                        TypeDefKind::Enum(variants) => {
                            variants.iter().map(|v| v.name.clone()).collect()
                        }
                        _ => Vec::new(),
                    }
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

}


/// Check if a type name is a generic type parameter
pub fn is_type_param(name: &str, generics: &[GenericParam]) -> bool {
    generics.iter().any(|g| g.name == name)
}

/// Check if a type name appears within a type (occurs check).
/// Prevents infinite recursion from self-referential type substitutions.
fn occurs_check(name: &str, ty: &Type, generics: &[GenericParam]) -> bool {
    match ty {
        Type::Name(n, args) => {
            if n == name { return true; }
            args.iter().any(|a| occurs_check(name, a, generics))
        }
        Type::Ref(_, inner) => occurs_check(name, inner, generics),
        Type::RefMut(_, inner) => occurs_check(name, inner, generics),
        Type::Option(inner) => occurs_check(name, inner, generics),
        Type::Result(ok, err) => occurs_check(name, ok, generics) || occurs_check(name, err, generics),
        Type::Tuple(elems) => elems.iter().any(|e| occurs_check(name, e, generics)),
        Type::Func(args, ret) => args.iter().any(|a| occurs_check(name, a, generics)) || occurs_check(name, ret, generics),
        Type::Shared(inner) => occurs_check(name, inner, generics),
        Type::LocalShared(inner) => occurs_check(name, inner, generics),
        Type::Weak(inner) => occurs_check(name, inner, generics),
        Type::WeakLocal(inner) => occurs_check(name, inner, generics),
        Type::RawPtr(inner) => occurs_check(name, inner, generics),
        Type::RawPtrMut(inner) => occurs_check(name, inner, generics),
        Type::CShared(inner) => occurs_check(name, inner, generics),
        Type::CBorrow(inner) => occurs_check(name, inner, generics),
        Type::CBorrowMut(inner) => occurs_check(name, inner, generics),
        Type::Newtype(_, inner) => occurs_check(name, inner, generics),
        Type::ExternFunc(args, ret) => args.iter().any(|a| occurs_check(name, a, generics)) || occurs_check(name, ret, generics),
        Type::CBuffer(inner) => occurs_check(name, inner, generics),
        Type::Array(inner, _) => occurs_check(name, inner, generics),
        Type::Slice(inner) => occurs_check(name, inner, generics),
        Type::Cap(_) | Type::Nothing | Type::RawString | Type::Allocator | Type::Infer
        | Type::ImplTrait(_) | Type::DynTrait(_) => false,
    }
}

/// Substitute type parameters in a type.
/// If substitution would cause infinite recursion (self-referential type),
/// returns the original type unchanged to let downstream checks catch the mismatch.
pub fn subst_type_params(ty: &Type, generics: &[GenericParam], type_map: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Name(name, args) => {
            if is_type_param(name, generics) {
                if let Some(concrete) = type_map.get(name) {
                    // Occurs check: if concrete type references this parameter,
                    // return original to prevent infinite recursion.
                    if occurs_check(name, concrete, generics) {
                        ty.clone()
                    } else {
                        concrete.clone()
                    }
                } else {
                    ty.clone()
                }
            } else {
                let new_args: Vec<Type> = args.iter()
                    .map(|a| subst_type_params(a, generics, type_map))
                    .collect();
                Type::Name(name.clone(), new_args)
            }
        }
        Type::Ref(_, inner) => Type::Ref(None, Box::new(subst_type_params(inner, generics, type_map))),
        Type::RefMut(_, inner) => Type::RefMut(None, Box::new(subst_type_params(inner, generics, type_map))),
        Type::Option(inner) => Type::Option(Box::new(subst_type_params(inner, generics, type_map))),
        Type::Result(ok, err) => Type::Result(
            Box::new(subst_type_params(ok, generics, type_map)),
            Box::new(subst_type_params(err, generics, type_map)),
        ),
        Type::Tuple(elems) => Type::Tuple(
            elems.iter().map(|e| subst_type_params(e, generics, type_map)).collect(),
        ),
        Type::Func(args, ret) => Type::Func(
            args.iter().map(|a| subst_type_params(a, generics, type_map)).collect(),
            Box::new(subst_type_params(ret, generics, type_map)),
        ),
        Type::Shared(inner) => Type::Shared(Box::new(subst_type_params(inner, generics, type_map))),
        Type::LocalShared(inner) => Type::LocalShared(Box::new(subst_type_params(inner, generics, type_map))),
        Type::Weak(inner) => Type::Weak(Box::new(subst_type_params(inner, generics, type_map))),
        Type::WeakLocal(inner) => Type::WeakLocal(Box::new(subst_type_params(inner, generics, type_map))),
        Type::RawPtr(inner) => Type::RawPtr(Box::new(subst_type_params(inner, generics, type_map))),
        Type::RawPtrMut(inner) => Type::RawPtrMut(Box::new(subst_type_params(inner, generics, type_map))),
        Type::CShared(inner) => Type::CShared(Box::new(subst_type_params(inner, generics, type_map))),
        Type::CBorrow(inner) => Type::CBorrow(Box::new(subst_type_params(inner, generics, type_map))),
        Type::CBorrowMut(inner) => Type::CBorrowMut(Box::new(subst_type_params(inner, generics, type_map))),
        Type::Newtype(name, inner) => Type::Newtype(name.clone(), Box::new(subst_type_params(inner, generics, type_map))),
        Type::Cap(_) | Type::Nothing | Type::RawString | Type::Allocator | Type::Infer => ty.clone(),
        Type::ExternFunc(args, ret) => Type::ExternFunc(
            args.iter().map(|a| subst_type_params(a, generics, type_map)).collect(),
            Box::new(subst_type_params(ret, generics, type_map)),
        ),
        Type::CBuffer(inner) => Type::CBuffer(Box::new(subst_type_params(inner, generics, type_map))),
        Type::Array(inner, size) => Type::Array(Box::new(subst_type_params(inner, generics, type_map)), *size),
        Type::Slice(inner) => Type::Slice(Box::new(subst_type_params(inner, generics, type_map))),
        Type::ImplTrait(traits) => Type::ImplTrait(traits.clone()),
            Type::DynTrait(traits) => Type::DynTrait(traits.clone()),
    }
}

pub(crate) fn same_type(a: &Type, b: &Type) -> bool {
    // Only treat 'unknown' as matching if BOTH sides are unknown.
    // Single-sided unknown would mask cascade errors — let the
    // real type propagate so subsequent checks detect mismatches.
    if matches!(a, Type::Name(n, _) if n == "unknown") && matches!(b, Type::Name(n, _) if n == "unknown") {
        return true;
    }
    // Normalize Type::Name("Result", [T, E]) <-> Type::Result(T, E) and Type::Name("Option", [T]) <-> Type::Option(T)
    // Compare args directly without cloning to allocate new enum variants.
    match (a, b) {
        (Type::Name(na, aa), Type::Name(nb, ab)) => na == nb && aa.len() == ab.len() && aa.iter().zip(ab.iter()).all(|(x, y)| same_type(x, y)),
        (Type::Name(n, args), Type::Result(ok, err)) if n == "Result" && args.len() == 2 => {
            same_type(&args[0], ok) && same_type(&args[1], err)
        }
        (Type::Result(ok, err), Type::Name(n, args)) if n == "Result" && args.len() == 2 => {
            same_type(ok, &args[0]) && same_type(err, &args[1])
        }
        (Type::Name(n, args), Type::Option(inner)) if n == "Option" && args.len() == 1 => {
            same_type(&args[0], inner)
        }
        (Type::Option(inner), Type::Name(n, args)) if n == "Option" && args.len() == 1 => {
            same_type(inner, &args[0])
        }
        (Type::Ref(_, a), Type::Ref(_, b)) => same_type(a, b),
        (Type::RefMut(_, a), Type::RefMut(_, b)) => same_type(a, b),
        (Type::Option(a), Type::Option(b)) => same_type(a, b),
        (Type::Result(a1, a2), Type::Result(b1, b2)) => same_type(a1, b1) && same_type(a2, b2),
        (Type::Tuple(a), Type::Tuple(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_type(x, y)),
        (Type::Func(a_args, a_ret), Type::Func(b_args, b_ret)) => {
            a_args.len() == b_args.len()
                && a_args.iter().zip(b_args.iter()).all(|(x, y)| same_type(x, y))
                && same_type(a_ret, b_ret)
        }
        (Type::Cap(a), Type::Cap(b)) => a == b,
        (Type::Shared(a), Type::Shared(b)) => same_type(a, b),
        (Type::LocalShared(a), Type::LocalShared(b)) => same_type(a, b),
        (Type::Weak(a), Type::Weak(b)) => same_type(a, b),
        (Type::WeakLocal(a), Type::WeakLocal(b)) => same_type(a, b),
        // Newtypes with same name and same inner type are equal
        (Type::Newtype(n1, a), Type::Newtype(n2, b)) => n1 == n2 && same_type(a, b),
        // A named type matches a newtype with the same inner type name
        (Type::Name(n, _), Type::Newtype(n2, _)) | (Type::Newtype(n2, _), Type::Name(n, _)) => {
            n == n2
        }
        (Type::Allocator, Type::Allocator) => true,
        (Type::Infer, Type::Infer) => true,
        (Type::Array(a_inner, a_size), Type::Array(b_inner, b_size)) => {
            a_size == b_size && same_type(a_inner, b_inner)
        }
        (Type::Slice(a), Type::Slice(b)) => same_type(a, b),
        (Type::ImplTrait(a), Type::ImplTrait(b)) => a == b,
        (Type::DynTrait(a), Type::DynTrait(b)) => a == b,
        (Type::Nothing, Type::Nothing) => true,
        (Type::RawString, Type::RawString) => true,
        (Type::ExternFunc(a_args, a_ret), Type::ExternFunc(b_args, b_ret)) => {
            a_args.len() == b_args.len()
                && a_args.iter().zip(b_args.iter()).all(|(x, y)| same_type(x, y))
                && same_type(a_ret, b_ret)
        }
        (Type::CBuffer(a), Type::CBuffer(b)) => same_type(a, b),
        (Type::RawPtr(a), Type::RawPtr(b)) => same_type(a, b),
        (Type::RawPtrMut(a), Type::RawPtrMut(b)) => same_type(a, b),
        (Type::CShared(a), Type::CShared(b)) => same_type(a, b),
        (Type::CBorrow(a), Type::CBorrow(b)) => same_type(a, b),
        (Type::CBorrowMut(a), Type::CBorrowMut(b)) => same_type(a, b),
        _ => false,
    }
}

/// Check if a concrete type can be coerced to a dyn Trait type (e.g., Circle → dyn Drawable)
/// `impls` maps (trait_name, type_name) -> method_names
fn is_trait_coercion(declared: &Type, init_ty: &Type, impls: &HashMap<(String, String), Vec<String>>) -> bool {
    match (declared, init_ty) {
        (Type::DynTrait(trait_names), Type::Name(ty_name, _)) => {
            trait_names.iter().all(|trait_name| {
                impls.contains_key(&(trait_name.clone(), ty_name.clone()))
            })
        }
        _ => false,
    }
}

pub(crate) fn is_int(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "i32" || n == "i64")
}

pub(crate) fn is_numeric(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "i32" || n == "i64" || n == "f64")
}

pub(crate) fn is_bool(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "bool")
}

pub(crate) fn is_string(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "string")
}

pub fn fmt_type(t: &Type) -> String {
    match t {
        Type::Name(n, args) if args.is_empty() => n.clone(),
        Type::Name(n, args) => format!("{}<{}>", n, args.iter().map(fmt_type).collect::<Vec<_>>().join(", ")),
        Type::Ref(lt, inner) => {
            if let Some(l) = lt { format!("&'{} {}", l, fmt_type(inner)) } else { format!("&{}", fmt_type(inner)) }
        }
        Type::RefMut(lt, inner) => {
            if let Some(l) = lt { format!("&'{} mut {}", l, fmt_type(inner)) } else { format!("&mut {}", fmt_type(inner)) }
        }
        Type::Option(inner) => format!("{}?", fmt_type(inner)),
        Type::Result(ok, err) => format!("Result<{}, {}>", fmt_type(ok), fmt_type(err)),
        Type::Tuple(elems) => format!("({})", elems.iter().map(fmt_type).collect::<Vec<_>>().join(", ")),
        Type::Func(args, ret) => format!(
            "fn({}) -> {}",
            args.iter().map(fmt_type).collect::<Vec<_>>().join(", "),
            fmt_type(ret)
        ),
        Type::Cap(name) => format!("cap {}", name),
        Type::Shared(inner) => format!("shared {}", fmt_type(inner)),
        Type::LocalShared(inner) => format!("local_shared {}", fmt_type(inner)),
        Type::Weak(inner) => format!("weak {}", fmt_type(inner)),
        Type::WeakLocal(inner) => format!("weak_local {}", fmt_type(inner)),
        Type::Newtype(name, inner) => format!("newtype {} {}", name, fmt_type(inner)),
        Type::Nothing => "nothing".to_string(),
        Type::Allocator => "Allocator".to_string(),
        Type::Array(inner, size) => format!("[{}; {}]", fmt_type(inner), size),
        Type::Slice(inner) => format!("[{}]", fmt_type(inner)),
        Type::ImplTrait(traits) => format!("impl {}", traits.join(" + ")),
        Type::DynTrait(traits) => format!("dyn {}", traits.join(" + ")),
        Type::RawPtr(inner) => format!("*{}", fmt_type(inner)),
        Type::RawPtrMut(inner) => format!("*mut {}", fmt_type(inner)),
        Type::CShared(inner) => format!("c_shared {}", fmt_type(inner)),
        Type::CBorrow(inner) => format!("c_borrow {}", fmt_type(inner)),
        Type::CBorrowMut(inner) => format!("c_borrow_mut {}", fmt_type(inner)),
        Type::RawString => "raw_string".to_string(),
        Type::Infer => "_".to_string(),
        Type::ExternFunc(args, ret) => {
            let args_str: Vec<String> = args.iter().map(fmt_type).collect();
            format!("extern \"C\" fn({}) -> {}", args_str.join(", "), fmt_type(ret))
        }
        Type::CBuffer(inner) => format!("CBuffer<{}>", fmt_type(inner)),
    }
}
