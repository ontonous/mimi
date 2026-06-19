mod value;
mod closure_utils;
mod eval;
mod call;
mod builtins;
mod ffi_call;
mod pattern;
mod quote;
mod actor;
pub mod error;
pub(crate) mod pool;

pub use value::*;
pub use error::InterpError;

use crate::ast::*;
use crate::ffi::FfiContract;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use closure_utils::collect_free_vars;

/// Internal loop control flow signal
#[derive(Debug, Clone)]
pub(crate) enum LoopAction {
    Continue,
    Break(Option<Value>),
}

pub struct Interpreter<'a> {
    file: &'a File,
    env: Vec<HashMap<String, Value>>,
    /// Track which variables have been moved (for move semantics)
    moved_vars: Vec<HashMap<String, bool>>,
    /// Track which variables are mutable
    mut_vars: Vec<HashMap<String, bool>>,
    constructors: HashMap<String, usize>,
    /// Set of constructor names that are newtypes (for wrapping result in Value::Newtype)
    newtype_constructors: HashMap<String, bool>,
    /// Maps type name to its variants (for Result/Option-like types)
    type_variants: HashMap<String, Vec<String>>,
    /// Variants that represent "failure" (Err, None, *Error, *Fail)
    failure_variants: HashMap<String, bool>,
    /// Capability definitions: cap_name -> list of component caps
    cap_defs: HashMap<String, Vec<String>>,
    /// Compensation stack for on failure blocks (LIFO) - scope-aware
    /// Each scope level contains compensation blocks registered in that scope
    /// Push a new scope when entering a block, pop when exiting
    compensation_stack: Vec<Vec<Vec<Stmt>>>,
    /// Arena memory blocks (arena_id -> Arena)
    arenas: Vec<Arena>,
    /// Current arena scope depth (track nesting for error messages)
    arena_depth: usize,
    /// Whether to verify contracts at runtime
    pub verify_contracts: bool,
    /// Whether to verify FFI contracts (requires/ensures) at runtime
    pub verify_ffi: bool,
    /// Trait definitions: trait_name -> TraitDef
    trait_defs: HashMap<String, TraitDef>,
    /// Trait implementations: type_name -> trait_name -> list of FuncDef methods
    type_impls: HashMap<String, HashMap<String, Vec<FuncDef>>>,
    /// Extern function declarations: func_name -> ExternFunc
    extern_funcs: HashMap<String, ExternFunc>,
    /// Pre-computed FFI contracts for extern functions.
    ffi_contracts: HashMap<String, FfiContract>,
    /// Type definitions for reflection: type_name -> (fields, variants)
    type_defs: HashMap<String, TypeDef>,
    /// Pre-computed results for comptime functions (no-arg functions evaluated at startup)
    comptime_results: HashMap<String, Value>,
    /// Loaded shared libraries: lib_path -> Library handle
    loaded_libs: Vec<libloading::Library>,
    /// Default allocator kind (set by --allocator CLI flag)
    pub default_allocator: AllocatorKind,
    /// Current loop control flow action (break/continue signal)
    loop_action: Option<LoopAction>,
    /// Early return signal for ? propagation (exception-like, preserves value)
    early_return: Option<Value>,
    /// Call stack for error context (function names being executed)
    call_stack: Vec<String>,
    /// Recursion depth guard to prevent stack overflow
    recursion_depth: usize,
    /// O(1) function lookup index: name -> FuncDef
    func_index: HashMap<String, FuncDef>,
    /// O(1) actor lookup index: name -> ActorDef
    actor_index: HashMap<String, ActorDef>,
}

impl<'a> Interpreter<'a> {
    pub fn new(file: &'a File) -> Self {
        let mut constructors = HashMap::new();
        let mut newtype_constructors = HashMap::new();
        let mut type_variants: HashMap<String, Vec<String>> = HashMap::new();
        let mut failure_variants: HashMap<String, bool> = HashMap::new();
        let mut cap_defs: HashMap<String, Vec<String>> = HashMap::new();
        for item in &file.items {
            Self::collect_constructors(item, &mut constructors, &mut newtype_constructors, &mut type_variants, &mut failure_variants);
            Self::collect_caps(item, &mut cap_defs);
        }
        // Register built-in Result/Option constructors
        constructors.insert("Ok".to_string(), 1);
        constructors.insert("Err".to_string(), 1);
        constructors.insert("Some".to_string(), 1);
        constructors.insert("None".to_string(), 0);
        // Also mark Err and None as failure variants for the ? operator
        failure_variants.insert("Err".to_string(), true);
        failure_variants.insert("None".to_string(), true);
        let mut trait_defs = HashMap::new();
        let mut type_impls: HashMap<String, HashMap<String, Vec<FuncDef>>> = HashMap::new();
        let mut extern_funcs: HashMap<String, ExternFunc> = HashMap::new();
        let mut ffi_contracts: HashMap<String, FfiContract> = HashMap::new();
        let mut type_defs: HashMap<String, TypeDef> = HashMap::new();
        for item in &file.items {
            Self::collect_traits(item, &mut trait_defs, &mut type_impls);
            Self::collect_type_defs(item, &mut type_defs);
        }
        // Build contracts after type_defs are populated so record type names are known
        for item in &file.items {
            Self::collect_extern_funcs(item, &mut extern_funcs, &mut ffi_contracts, &cap_defs, &type_defs);
        }
        // Expand built-in derive macros
        Self::expand_derives(&type_defs, &mut trait_defs, &mut type_impls);
        // Build O(1) function and actor lookup indices
        let mut func_index = HashMap::new();
        let mut actor_index = HashMap::new();
        Self::build_func_index(&file.items, &mut func_index);
        Self::build_actor_index(&file.items, &mut actor_index);
        Self {
            file,
            env: vec![HashMap::new()],
            moved_vars: vec![HashMap::new()],
            mut_vars: vec![HashMap::new()],
            constructors,
            newtype_constructors,
            type_variants,
            failure_variants,
            cap_defs,
            compensation_stack: Vec::new(),
            arenas: Vec::new(),
            arena_depth: 0,
            verify_contracts: true,
            verify_ffi: true,
            trait_defs,
            type_impls,
            extern_funcs,
            ffi_contracts,
            type_defs,
            comptime_results: HashMap::new(),
            loaded_libs: Vec::new(),
            default_allocator: AllocatorKind::System,
            loop_action: None,
            early_return: None,
            call_stack: Vec::new(),
            recursion_depth: 0,
            func_index,
            actor_index,
        }
    }

    const MAX_RECURSION_DEPTH: usize = 4096;

    fn with_depth_check<F, T>(&mut self, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut Self) -> Result<T, String>,
    {
        if self.recursion_depth >= Self::MAX_RECURSION_DEPTH {
            return Err("recursion limit exceeded (possible infinite recursion)".into());
        }
        self.recursion_depth += 1;
        let result = f(self);
        self.recursion_depth = self.recursion_depth.saturating_sub(1);
        result
    }

    fn build_func_index(items: &[Item], index: &mut HashMap<String, FuncDef>) {
        Self::build_func_index_rec(items, "", index);
    }

    fn build_func_index_rec(items: &[Item], prefix: &str, index: &mut HashMap<String, FuncDef>) {
        for item in items {
            match item {
                Item::Func(f) => {
                    // Store by unqualified name (first wins)
                    index.entry(f.name.clone()).or_insert_with(|| f.clone());
                    // Store by qualified name
                    if !prefix.is_empty() {
                        let qualified = format!("{}::{}", prefix, f.name);
                        index.entry(qualified).or_insert_with(|| f.clone());
                    }
                }
                Item::Module(m) => {
                    let new_prefix = if prefix.is_empty() {
                        m.name.clone()
                    } else {
                        format!("{}::{}", prefix, m.name)
                    };
                    Self::build_func_index_rec(&m.items, &new_prefix, index);
                }
                _ => {}
            }
        }
    }

    fn build_actor_index(items: &[Item], index: &mut HashMap<String, ActorDef>) {
        for item in items {
            match item {
                Item::Actor(a) => { index.insert(a.name.clone(), a.clone()); }
                Item::Module(m) => Self::build_actor_index(&m.items, index),
                _ => {}
            }
        }
    }

    fn collect_constructors(item: &Item, out: &mut HashMap<String, usize>, newtype_constructors: &mut HashMap<String, bool>, type_variants: &mut HashMap<String, Vec<String>>, failure_variants: &mut HashMap<String, bool>) {
        match item {
            Item::Type(t) => {
                match &t.kind {
                    TypeDefKind::Enum(variants) => {
                        let mut variant_names = Vec::new();
                        for v in variants {
                            let arity = match &v.payload {
                                None => 0,
                                Some(VariantPayload::Tuple(types)) => types.len(),
                                Some(VariantPayload::Record(fields)) => fields.len(),
                            };
                            out.insert(v.name.clone(), arity);
                            variant_names.push(v.name.clone());
                            // Mark failure-like variants
                            let name_lower = v.name.to_lowercase();
                            if name_lower == "err" || name_lower == "none" || name_lower.ends_with("error") || name_lower.ends_with("fail") {
                                failure_variants.insert(v.name.clone(), true);
                            }
                        }
                        type_variants.insert(t.name.clone(), variant_names);
                    }
                    TypeDefKind::Newtype(_) => {
                        out.insert(t.name.clone(), 1);
                        newtype_constructors.insert(t.name.clone(), true);
                    }
                    _ => {}
                }
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_constructors(inner, out, newtype_constructors, type_variants, failure_variants);
                }
            }
            Item::Trait(_) | Item::Impl(_) => {
                // Traits and impls don't define constructors
            }
            _ => {}
        }
    }

    fn collect_extern_funcs(
        item: &Item,
        out: &mut HashMap<String, ExternFunc>,
        contracts: &mut HashMap<String, FfiContract>,
        cap_defs: &HashMap<String, Vec<String>>,
        type_defs: &HashMap<String, TypeDef>,
    ) {
        let cap_names: std::collections::HashSet<String> = cap_defs.keys().cloned().collect();
        let record_type_names: std::collections::HashSet<String> = type_defs.iter()
            .filter(|(_, td)| matches!(td.kind, TypeDefKind::Record(_)))
            .map(|(name, _)| name.clone())
            .collect();
        match item {
            Item::ExternBlock(block) => {
                for func in &block.funcs {
                    out.insert(func.name.clone(), func.clone());
                    contracts.insert(func.name.clone(), FfiContract::from_extern_with_caps(func, &cap_names, &record_type_names));
                }
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_extern_funcs(inner, out, contracts, cap_defs, type_defs);
                }
            }
            _ => {}
        }
    }

    fn collect_type_defs(item: &Item, out: &mut HashMap<String, TypeDef>) {
        match item {
            Item::Type(t) => {
                out.insert(t.name.clone(), t.clone());
            }
            Item::Actor(actor) => {
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
                out.insert(actor.name.clone(), actor_type_def);
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_type_defs(inner, out);
                }
            }
            _ => {}
        }
    }

    /// Expand built-in derive macros for types
    fn expand_derives(type_defs: &HashMap<String, TypeDef>, _trait_defs: &mut HashMap<String, TraitDef>, type_impls: &mut HashMap<String, HashMap<String, Vec<FuncDef>>>) {
        for (type_name, type_def) in type_defs {
            for derive_name in &type_def.derives {
                match derive_name.as_str() {
                    "Debug" => {
                        // Generate to_string method for Debug
                        let to_string_func = FuncDef {
                            name: "to_string".to_string(),
                            commitment: Commitment::None,
                            pub_: false,
                            params: vec![],
                            ret: Some(Type::Name("string".into(), vec![])),
                            body: vec![],
                            where_clause: None,
                            generics: vec![],
                            effects: vec![],
                            is_comptime: false,
                            is_async: false,
                            extern_abi: None,
                            pos: (0, 0),
                        };
                        type_impls
                            .entry(type_name.clone())
                            .or_default()
                            .entry("Debug".to_string())
                            .or_default()
                            .push(to_string_func);
                    }
                    "Clone" => {
                        // Generate clone method for Clone
                        let clone_func = FuncDef {
                            name: "clone".to_string(),
                            commitment: Commitment::None,
                            pub_: false,
                            params: vec![],
                            ret: Some(Type::Name(type_name.clone(), vec![])),
                            body: vec![],
                            where_clause: None,
                            generics: vec![],
                            effects: vec![],
                            is_comptime: false,
                            is_async: false,
                            extern_abi: None,
                            pos: (0, 0),
                        };
                        type_impls
                            .entry(type_name.clone())
                            .or_default()
                            .entry("Clone".to_string())
                            .or_default()
                            .push(clone_func);
                    }
                    "Eq" => {
                        // Generate eq method for Eq
                        let eq_func = FuncDef {
                            name: "eq".to_string(),
                            commitment: Commitment::None,
                            pub_: false,
                            params: vec![Param {
                                name: "other".to_string(),
                                ty: Type::Name(type_name.clone(), vec![]),
                                mut_: false,
                            }],
                            ret: Some(Type::Name("bool".into(), vec![])),
                            body: vec![],
                            where_clause: None,
                            generics: vec![],
                            effects: vec![],
                            is_comptime: false,
                            is_async: false,
                            extern_abi: None,
                            pos: (0, 0),
                        };
                        type_impls
                            .entry(type_name.clone())
                            .or_default()
                            .entry("Eq".to_string())
                            .or_default()
                            .push(eq_func);
                    }
                    _ => {}
                }
            }
        }
    }

    fn collect_traits(item: &Item, trait_defs: &mut HashMap<String, TraitDef>, type_impls: &mut HashMap<String, HashMap<String, Vec<FuncDef>>>) {
        match item {
            Item::Trait(trait_def) => {
                trait_defs.insert(trait_def.name.clone(), trait_def.clone());
            }
            Item::Impl(impl_def) => {
                type_impls
                    .entry(impl_def.type_name.clone())
                    .or_default()
                    .insert(impl_def.trait_name.clone(), impl_def.methods.clone());
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_traits(inner, trait_defs, type_impls);
                }
            }
            _ => {}
        }
    }

    fn collect_caps(item: &Item, out: &mut HashMap<String, Vec<String>>) {
        match item {
            Item::Cap(cap) => {
                let components = if let Some(ref combined) = cap.combined_with {
                    // Parse "A + B" format
                    let parts: Vec<String> = combined.split(" + ")
                        .map(|s| s.trim().to_string())
                        .collect();
                    if parts.len() > 1 {
                        parts
                    } else {
                        vec![cap.name.clone(), combined.clone()]
                    }
                } else {
                    vec![cap.name.clone()]
                };
                out.insert(cap.name.clone(), components);
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_caps(inner, out);
                }
            }
            _ => {}
        }
    }

    /// Get the type name of a runtime value
    fn value_type_name(&self, val: &Value) -> String {
        match val {
            Value::Int(_) => "i32".into(),
            Value::Float(_) => "f64".into(),
            Value::Bool(_) => "bool".into(),
            Value::String(_) => "string".into(),
            Value::Unit => "unit".into(),
            Value::List(_) => "list".into(),
            Value::Array(_) => "array".into(),
            Value::Tuple(_) => "tuple".into(),
            Value::Variant(name, _) => name.clone(),
            Value::Record(Some(name), _) => name.clone(),
            Value::Record(None, _) => "record".into(),
            Value::Error(_) => "error".into(),
            Value::Newtype(v) => self.value_type_name(v),
            Value::Type(name) => name.clone(),
            Value::Closure { .. } => "closure".into(),
            Value::QuoteAst(_) => "AST".into(),
            Value::Shared(_) => "shared".into(),
            Value::LocalShared(_) => "local_shared".into(),
            Value::Ref(_) => "ref".into(),
            Value::RefMut(_) => "ref_mut".into(),
            Value::Cap(_) => "cap".into(),
            Value::Actor(_) => "actor".into(),
            Value::Future(_) => "future".into(),
            Value::ArenaRef(_, _) => "arena_ref".into(),
            Value::ArenaBlock(_) => "arena_block".into(),
            Value::WeakShared(_) | Value::WeakLocal(_) => "weak".into(),
            Value::Allocator(_) => "Allocator".into(),
            Value::Slice { .. } => "slice".into(),
            Value::Range { .. } => "range".into(),
            Value::CBuffer(_) => "CBuffer".into(),
            Value::DynTrait { trait_names, .. } => format!("dyn {}", trait_names.join(" + ")),
        }
    }

    /// Resolve a Type AST node to a type name string
    fn resolve_type_name(&self, ty: &Type) -> String {
        match ty {
            Type::Name(name, _) => name.clone(),
            Type::Ref(lt, inner) => {
                if let Some(l) = lt { format!("&'{} {}", l, self.resolve_type_name(inner)) } else { format!("&{}", self.resolve_type_name(inner)) }
            }
            Type::RefMut(lt, inner) => {
                if let Some(l) = lt { format!("&'{} mut {}", l, self.resolve_type_name(inner)) } else { format!("&mut {}", self.resolve_type_name(inner)) }
            }
            Type::Option(inner) => format!("Option<{}>", self.resolve_type_name(inner)),
            Type::Result(ok, err) => format!("Result<{}, {}>", self.resolve_type_name(ok), self.resolve_type_name(err)),
            Type::Tuple(elems) => {
                let names: Vec<String> = elems.iter().map(|e| self.resolve_type_name(e)).collect();
                format!("({})", names.join(", "))
            }
            Type::Func(args, ret) => {
                let arg_names: Vec<String> = args.iter().map(|a| self.resolve_type_name(a)).collect();
                format!("({}) -> {}", arg_names.join(", "), self.resolve_type_name(ret))
            }
            Type::Cap(name) => format!("cap {}", name),
            Type::Shared(inner) => format!("shared {}", self.resolve_type_name(inner)),
            Type::LocalShared(inner) => format!("local_shared {}", self.resolve_type_name(inner)),
            Type::Weak(inner) => format!("weak {}", self.resolve_type_name(inner)),
            Type::WeakLocal(inner) => format!("weak_local {}", self.resolve_type_name(inner)),
            Type::RawPtr(inner) => format!("*{}", self.resolve_type_name(inner)),
            Type::RawPtrMut(inner) => format!("*mut {}", self.resolve_type_name(inner)),
            Type::CShared(inner) => format!("c_shared {}", self.resolve_type_name(inner)),
            Type::CBorrow(inner) => format!("c_borrow {}", self.resolve_type_name(inner)),
            Type::CBorrowMut(inner) => format!("c_borrow_mut {}", self.resolve_type_name(inner)),
            Type::RawString => "raw_string".into(),
            Type::Infer => "_".into(),
            Type::ExternFunc(args, ret) => {
                let args_str: Vec<String> = args.iter().map(|a| self.resolve_type_name(a)).collect();
                format!("extern \"C\" fn({}) -> {}", args_str.join(", "), self.resolve_type_name(ret))
            }
            Type::Newtype(name, _) => name.clone(),
            Type::Nothing => "nothing".into(),
            Type::Allocator => "Allocator".into(),
            Type::Array(inner, size) => format!("[{}; {}]", self.resolve_type_name(inner), size),
            Type::Slice(inner) => format!("[{}]", self.resolve_type_name(inner)),
            Type::ImplTrait(traits) => format!("impl {}", traits.join(" + ")),
            Type::DynTrait(traits) => format!("dyn {}", traits.join(" + ")),
            Type::CBuffer(inner) => format!("CBuffer<{}>", self.resolve_type_name(inner)),
        }
    }

    /// Get type info for a type name
    fn type_info_for(&self, type_name: &str) -> Result<Value, String> {
        if let Some(type_def) = self.type_defs.get(type_name) {
            let mut fields_map = HashMap::new();
            match &type_def.kind {
                TypeDefKind::Record(fields) => {
                    for f in fields {
                        let field_info = vec![
                            (Value::String("name".into()), Value::String(f.name.clone())),
                            (Value::String("type".into()), Value::String(self.resolve_type_name(&f.ty))),
                        ];
                        fields_map.insert(f.name.clone(), Value::Tuple(field_info.into_iter().map(|(_, v)| v).collect()));
                    }
                }
                TypeDefKind::Enum(variants) => {
                    for v in variants {
                        let variant_info = vec![
                            Value::String(v.name.clone()),
                            Value::Bool(v.payload.is_some()),
                        ];
                        fields_map.insert(v.name.clone(), Value::Tuple(variant_info));
                    }
                }
                TypeDefKind::Alias(ty) => {
                    fields_map.insert("alias_of".into(), Value::String(self.resolve_type_name(ty)));
                }
                TypeDefKind::Newtype(ty) => {
                    fields_map.insert("inner".into(), Value::String(self.resolve_type_name(ty)));
                }
                TypeDefKind::Union(fields) => {
                    for f in fields {
                        let field_info = vec![
                            (Value::String("name".into()), Value::String(f.name.clone())),
                            (Value::String("type".into()), Value::String(self.resolve_type_name(&f.ty))),
                        ];
                        fields_map.insert(f.name.clone(), Value::Tuple(field_info.into_iter().map(|(_, v)| v).collect()));
                    }
                }
            }
            let mut info = HashMap::new();
            info.insert("name".into(), Value::String(type_name.into()));
            info.insert("fields".into(), Value::List(fields_map.into_values().collect()));
            Ok(Value::Record(None, info))
        } else {
            Err(format!("unknown type '{}'", type_name))
        }
    }

    pub fn run(&mut self) -> Result<Value, InterpError> {
        // Evaluate comptime functions (no-arg) at startup
        self.eval_comptime_funcs().map_err(|e| self.interp_err(e))?;
        let main = self.find_function("main")
            .ok_or_else(|| self.interp_err("no main() function found".into()))?;
        self.call_func(&main, vec![])
            .map_err(|e| self.interp_err(e))
    }

    /// Evaluate comptime functions with no arguments at startup
    fn eval_comptime_funcs(&mut self) -> Result<(), String> {
        let funcs: Vec<FuncDef> = self.file.items.iter().filter_map(|item| {
            match item {
                Item::Func(f) if f.is_comptime && f.params.is_empty() => Some(f.clone()),
                _ => None,
            }
        }).collect();
        for func in funcs {
            let result = self.call_func(&func, vec![])?;
            self.comptime_results.insert(func.name.clone(), result);
        }
        Ok(())
    }

    fn find_function(&self, name: &str) -> Option<FuncDef> {
        // O(1) lookup via pre-built index — try both qualified and unqualified
        self.func_index.get(name).cloned()
            .or_else(|| self.func_index.values()
                .find(|f| f.name == name)
                .cloned())
    }

    /// Build a qualified path from nested Field(Ident(...), ...) expressions
    fn build_qualified_path(obj: &Expr, field: &str) -> Option<String> {
        match obj {
            Expr::Ident(name) => Some(format!("{}::{}", name, field)),
            Expr::Field(inner_obj, inner_field) => {
                Self::build_qualified_path(inner_obj, inner_field).map(|base| format!("{}::{}", base, field))
            }
            _ => None,
        }
    }

    fn find_function_in_module(module: &ModuleDef, prefix: &str, name: &str) -> Option<FuncDef> {
        let current_prefix = if prefix.is_empty() {
            module.name.clone()
        } else {
            format!("{}::{}", prefix, module.name)
        };
        for inner in &module.items {
            match inner {
                Item::Func(f) => {
                    let qualified = format!("{}::{}", current_prefix, f.name);
                    if qualified == name || f.name == name {
                        return Some(f.clone());
                    }
                }
                Item::Module(m) => {
                    if let Some(f) = Self::find_function_in_module(m, &current_prefix, name) {
                        return Some(f);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn find_actor(&self, name: &str) -> Option<ActorDef> {
        // O(1) lookup via pre-built index
        self.actor_index.get(name).cloned()
    }

    fn push_scope(&mut self) {
        self.env.push(HashMap::new());
        self.moved_vars.push(HashMap::new());
        self.mut_vars.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.env.pop();
        self.moved_vars.pop();
        self.mut_vars.pop();
    }

    fn push_call(&mut self, func_name: &str) {
        self.call_stack.push(func_name.to_string());
    }

    fn pop_call(&mut self) {
        self.call_stack.pop();
    }

    /// Convert a string error into an InterpError with current call stack context.
    fn interp_err(&self, msg: String) -> InterpError {
        InterpError::new(msg).with_call_stack(self.call_stack.clone())
    }

    /// Convert a string error with operation context into an InterpError.
    fn interp_err_op(&self, msg: String, op: &str) -> InterpError {
        InterpError::with_op(msg, op).with_call_stack(self.call_stack.clone())
    }

    fn bind(&mut self, name: &str, value: Value) {
        self.env.last_mut().expect("scope stack non-empty").insert(name.into(), value);
        self.moved_vars.last_mut().expect("scope stack non-empty").insert(name.into(), false);
        // Default to immutable unless explicitly marked as mutable
        self.mut_vars.last_mut().expect("scope stack non-empty").entry(name.into()).or_insert(false);
    }

    fn bind_mut(&mut self, name: &str, value: Value) {
        self.env.last_mut().expect("scope stack non-empty").insert(name.into(), value);
        self.moved_vars.last_mut().expect("scope stack non-empty").insert(name.into(), false);
        self.mut_vars.last_mut().expect("scope stack non-empty").insert(name.into(), true);
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        for (scope, moved) in self.env.iter().zip(self.moved_vars.iter()).rev() {
            if let Some(v) = scope.get(name) {
                if moved.get(name).copied().unwrap_or(false) {
                    return None; // Treat moved vars as undefined
                }
                return Some(v.clone());
            }
        }
        None
    }

    fn is_moved(&self, name: &str) -> bool {
        for moved in self.moved_vars.iter().rev() {
            if let Some(&m) = moved.get(name) {
                return m;
            }
        }
        false
    }

    fn mark_moved(&mut self, name: &str) {
        for moved in self.moved_vars.iter_mut().rev() {
            if moved.contains_key(name) {
                moved.insert(name.into(), true);
                return;
            }
        }
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<(), String> {
        for (scope, moved) in self.env.iter_mut().zip(self.moved_vars.iter_mut()).rev() {
            if scope.contains_key(name) {
                // Check if variable is mutable
                for mut_scope in self.mut_vars.iter().rev() {
                    if let Some(&is_mut) = mut_scope.get(name) {
                        if !is_mut {
                            return Err(format!("cannot assign to immutable variable '{}'", name));
                        }
                        break;
                    }
                }
                scope.insert(name.into(), value);
                moved.insert(name.into(), false);
                return Ok(());
            }
        }
        Err(format!("undefined variable '{}' in assignment", name))
    }

    /// Push a new compensation scope level
    fn push_compensation_scope(&mut self) {
        self.compensation_stack.push(Vec::new());
    }

    /// Pop the current compensation scope level
    /// If run_compensations is true, execute all compensations in LIFO order before popping
    fn pop_compensation_scope(&mut self, run_compensations: bool) {
        if run_compensations {
            // Run compensation blocks in LIFO order for the current scope
            if let Some(scope) = self.compensation_stack.pop() {
                // Execute compensations in reverse order (LIFO within this scope)
                // Note: compensation_stack order is already LIFO across scopes,
                // but within a scope we want to execute in registration order (first registered = last executed)
                for block in scope.iter().rev() {
                    for stmt in block {
                        if let Err(e) = self.eval_stmt(stmt) {
                            eprintln!("compensation error: {} (ignored)", e);
                        }
                    }
                }
            }
        } else {
            // Just discard the scope (normal exit)
            self.compensation_stack.pop();
        }
    }

    /// Run all compensation blocks across all scope levels in LIFO order
    /// Used when propagation an error up through nested scopes
    fn run_all_compensations(&mut self) {
        // Run all remaining compensations in LIFO order
        while let Some(scope) = self.compensation_stack.pop() {
            for block in scope.iter().rev() {
                for stmt in block {
                    if let Err(e) = self.eval_stmt(stmt) {
                        eprintln!("compensation error: {} (ignored)", e);
                    }
                }
            }
        }
    }
}
