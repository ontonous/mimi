use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

use super::borrow::BorrowState;

pub(crate) struct Checker<'a> {
    pub(crate) file: &'a File,
    pub(crate) errors: Vec<Diagnostic>,
    pub(crate) warnings: Vec<Diagnostic>,
    pub(crate) funcs: HashMap<String, (Vec<Type>, Type)>,
    pub(crate) aliases: HashMap<String, Type>,
    pub(crate) types: HashMap<String, TypeDef>,
    /// Track newtype definitions: name -> inner type (unresolved)
    pub(crate) newtypes: HashMap<String, Type>,
    /// Track linear capabilities in scope: name -> consumed
    pub(crate) cap_vars: Vec<HashMap<String, bool>>,
    /// Track borrow state of variables: name -> borrow state
    pub(crate) borrows: Vec<HashMap<String, BorrowState>>,
    /// Track trait definitions: trait_name -> list of method names
    pub(crate) traits: HashMap<String, Vec<String>>,
    /// Track trait generic params: trait_name -> list of generic param names
    pub(crate) trait_generics: HashMap<String, Vec<String>>,
    /// Track trait implementations: (trait_name, type_name) -> list of method names
    pub(crate) impls: HashMap<(String, String), Vec<String>>,
    /// Track where clauses for functions: func_name -> (type_param, bounds)
    pub(crate) where_clauses: HashMap<String, (String, Vec<String>)>,
    /// Track effects for functions: func_name -> list of effect names
    pub(crate) func_effects: HashMap<String, Vec<String>>,
    /// Track available effects in current scope
    pub(crate) available_effects: Vec<HashMap<String, bool>>,
    /// Strict mode: enforce $$ lock semantics
    pub(crate) strict: bool,
    /// Track variable scopes for shadowing detection
    pub(crate) var_scopes: Vec<HashMap<String, usize>>,
    /// Track mutable variables: name -> is_mut
    pub(crate) mut_vars: Vec<HashMap<String, bool>>,
    /// Track generic parameters per function: func_name -> generic params
    pub(crate) func_generics: HashMap<String, Vec<GenericParam>>,
    /// Track generic parameters per type def: type_name -> generic params
    pub(crate) type_generics: HashMap<String, Vec<GenericParam>>,
    /// Track methods available on types via traits: type_name -> list of (trait_name, method_name)
    pub(crate) type_methods: HashMap<String, Vec<(String, String)>>,
    /// Track trait method signatures: (trait_name, method_name) -> (param_types, return_type)
    pub(crate) trait_method_sigs: HashMap<(String, String), (Vec<Type>, Type)>,
    /// Track imported module names (from `use` statements)
    pub(crate) use_imports: Vec<String>,
    /// Track current module path for qualified names
    pub(crate) module_path: Vec<String>,
    /// Track loop nesting depth for break/continue validation
    pub(crate) loop_depth: usize,
    /// Track generic parameters in scope while checking signatures
    pub(crate) generic_scope: Vec<String>,
    /// Track arena block nesting depth for escape detection
    pub(crate) arena_depth: usize,
    /// Current item/function line-col for fallback error positioning
    pub(crate) current_line: usize,
    pub(crate) current_col: usize,
}

impl<'a> Checker<'a> {
    pub(crate) fn new(file: &'a File) -> Self {
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
            arena_depth: 0,
            current_line: 0,
            current_col: 0,
        }
    }

    /// Set the current position for fallback error spans.
    pub(crate) fn set_pos(&mut self, line: usize, col: usize) {
        self.current_line = line;
        self.current_col = col;
    }

    pub(crate) fn check(&mut self) -> Result<(), Vec<Diagnostic>> {
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

    pub(crate) fn emit_code(&mut self, code: &str, msg: impl Into<String>) {
        let span = Span::single(self.current_line, self.current_col);
        self.errors.push(Diagnostic::error_code(code, msg, span));
    }

    pub(crate) fn emit_warning_code(&mut self, code: &str, msg: impl Into<String>) {
        let span = Span::single(self.current_line, self.current_col);
        self.warnings.push(Diagnostic::warning_code(code, msg, span));
    }
}

mod borrow;
mod types;
mod items;
mod func;
mod pattern;
mod generics;
mod vars;
