//! Flow-based Mimi module loader — iterative worklist approach.
//!
//! Replaces `&mut self` recursion with a flat BFS worklist:
//! no state machine enum, no `&mut self`, just explicit state threading.
//!
//! Entry points:
//!   - `flow_load_main(acc, path) -> (acc, LoadedModule)` — load main file + all transitive deps
//!   - `flow_load_file(acc, path) -> (acc, LoadedModule)` — load a single file (cached)

use crate::ast::*;

type ResolvedImport = (Vec<String>, PathBuf);
type ImportEdge = (PathBuf, Vec<ResolvedImport>);
use crate::lexer;
use crate::lockfile;
use crate::manifest;
use crate::parser;
use crate::span::{
    SourceId, SourceIdRemap, SourceKey, SourceRecord, SourceRegistry, SourceTextOrigin,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ── Import public types from parent module ─────────────────────────────

use super::LoadedModule;

// ── Accumulator (replaces &mut self on ModuleLoader) ────────────────────

#[derive(Clone, Debug, Default)]
pub struct Acc {
    pub loaded: HashMap<PathBuf, LoadedModule>,
    pub modules: HashMap<String, LoadedModule>,
    pub dep_paths: HashMap<String, PathBuf>,
    pub lock_entries: HashMap<String, lockfile::LockEntry>,
    pub base_dir: PathBuf,
    workspace_root: Option<PathBuf>,
    dependency_identities: HashMap<String, String>,
    source_ids: HashMap<PathBuf, SourceId>,
    source_registry: SourceRegistry,
}

impl Acc {
    pub fn new(base_dir: PathBuf) -> Self {
        let mut acc = Acc {
            base_dir,
            ..Default::default()
        };
        acc.init_deps();
        acc
    }

    fn init_deps(&mut self) {
        if let Ok(Some((dir, manifest))) = manifest::Manifest::find(&self.base_dir) {
            self.workspace_root = Some(normalize_source_path(&dir));
            if let Some(deps) = &manifest.dependencies {
                for dep in deps {
                    if let Some(path_str) = &dep.path {
                        // Path deps may use ../sibling (monorepo). Absolute/NUL rejected.
                        let dep_path = match crate::path_safety::resolve_path_dep(&dir, path_str) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        if dep_path.exists() {
                            let identity = manifest::Manifest::find(&dep_path)
                                .ok()
                                .flatten()
                                .and_then(|(_, manifest)| manifest.package)
                                .map(|package| {
                                    package
                                        .version
                                        .map(|version| format!("{}@{version}", package.name))
                                        .unwrap_or(package.name)
                                })
                                .or_else(|| {
                                    dep.version
                                        .as_ref()
                                        .map(|version| format!("{}@{version}", dep.name))
                                })
                                .unwrap_or_else(|| dep.name.clone());
                            self.dependency_identities
                                .insert(dep.name.clone(), identity);
                            self.dep_paths
                                .insert(dep.name.clone(), normalize_source_path(&dep_path));
                        }
                    }
                }
            }
            if let Ok(Some(lf)) = lockfile::Lockfile::load(&dir) {
                for entry in lf.package {
                    self.lock_entries.insert(entry.name.clone(), entry);
                }
            }
        }
    }

    fn module_key(&self, path: &Path) -> String {
        path.strip_prefix(&self.base_dir)
            .or_else(|_| path.strip_prefix(std::env::current_dir().unwrap_or_default()))
            .unwrap_or(path)
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/")
    }

    pub(crate) fn with_source_registry(
        mut self,
        source_ids: HashMap<PathBuf, SourceId>,
        source_registry: SourceRegistry,
    ) -> Self {
        self.source_ids = source_ids;
        self.source_registry = source_registry;
        self
    }

    pub(crate) fn into_source_registry(self) -> (HashMap<PathBuf, SourceId>, SourceRegistry) {
        (self.source_ids, self.source_registry)
    }

    fn source_key(&self, path: &Path) -> SourceKey {
        let path = normalize_source_path(path);

        if let Some(std_root) = super::stdlib_dir().map(|root| normalize_source_path(&root)) {
            if let Ok(relative) = path.strip_prefix(&std_root) {
                let relative = source_relative_path(relative);
                return SourceKey::new(format!("stdlib:{relative}"))
                    .expect("stdlib source key is non-empty");
            }
        }

        let mut dependencies: Vec<_> = self.dep_paths.iter().collect();
        dependencies.sort_by(|left, right| left.0.cmp(right.0));
        for (name, root) in dependencies {
            if let Ok(relative) = path.strip_prefix(root) {
                let relative = source_relative_path(relative);
                let identity = self
                    .dependency_identities
                    .get(name)
                    .cloned()
                    .or_else(|| {
                        self.lock_entries
                            .get(name)
                            .map(|entry| format!("{name}@{}", entry.version))
                    })
                    .unwrap_or_else(|| name.clone());
                return SourceKey::new(format!("package:{identity}:{relative}"))
                    .expect("dependency source key is non-empty");
            }
        }

        let project_root = self.workspace_root.as_ref().unwrap_or(&self.base_dir);
        let deps_root = normalize_source_path(&project_root.join(".mimi").join("deps"));
        if let Ok(relative) = path.strip_prefix(&deps_root) {
            let mut components = relative.components();
            if let Some(package) = components.next() {
                let package = package.as_os_str().to_string_lossy();
                let module: PathBuf = components.collect();
                let module = source_relative_path(&module);
                let version = self
                    .lock_entries
                    .get(package.as_ref())
                    .map(|entry| format!("@{}", entry.version))
                    .unwrap_or_default();
                return SourceKey::new(format!("package:{package}{version}:{module}"))
                    .expect("cached dependency source key is non-empty");
            }
        }

        if let Some(root) = &self.workspace_root {
            if let Ok(relative) = path.strip_prefix(root) {
                let relative = source_relative_path(relative);
                return SourceKey::new(format!("workspace:{relative}"))
                    .expect("workspace source key is non-empty");
            }
        }

        // `base_dir` may be a document parent rather than the workspace root.
        // Without a manifest, a base-relative key would drift when another
        // entry point is selected. The canonical URI hash is honest and stable
        // for that disk source without exposing the absolute host path.
        SourceKey::external_uri(&source_file_uri(&path))
    }

    fn source_origin(&self, path: &Path, requested: SourceTextOrigin) -> SourceTextOrigin {
        if requested == SourceTextOrigin::Memory {
            return requested;
        }
        let path = normalize_source_path(path);
        if super::stdlib_dir()
            .map(|root| normalize_source_path(&root))
            .is_some_and(|root| path.starts_with(root))
        {
            SourceTextOrigin::Builtin
        } else {
            requested
        }
    }

    pub(super) fn source_id_for(
        mut self,
        path: &Path,
        text_origin: SourceTextOrigin,
    ) -> Result<(Self, SourceId), String> {
        let path = normalize_source_path(path);
        if let Some(source_id) = self.source_ids.get(&path).copied() {
            return Ok((self, source_id));
        }
        if let Some(source_id) = self.source_registry.id_for_disk_path(&path) {
            self.source_ids.insert(path, source_id);
            return Ok((self, source_id));
        }
        let source_key = self.source_key(&path);
        let text_origin = self.source_origin(&path, text_origin);
        let record = SourceRecord::new(source_key, text_origin)
            .with_uri(source_file_uri(&path))
            .with_disk_path(path.clone());
        let source_id = self
            .source_registry
            .register(record)
            .map_err(|error| error.to_string())?;
        self.source_ids.insert(path, source_id);
        Ok((self, source_id))
    }
}

fn source_relative_path(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if value.is_empty() {
        "<root>".to_string()
    } else {
        value
    }
}

fn source_file_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy().replace('\\', "/"))
}

fn normalize_source_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn remap_file_into_registry(file: &mut File, target: &mut SourceRegistry) -> Result<(), String> {
    let remap = target
        .merge_from(&file.sources)
        .map_err(|error| format!("cannot merge source registries: {error}"))?;
    remap_file_spans(file, &remap)?;
    file.sources = target.clone();
    Ok(())
}

fn remap_file_spans(file: &mut File, remap: &SourceIdRemap) -> Result<(), String> {
    remap_imports_spans(&mut file.imports, remap)?;
    for item in &mut file.items {
        remap_item_spans(item, remap)?;
    }
    Ok(())
}

fn remap_meta(meta: &mut AstNodeMeta, remap: &SourceIdRemap) -> Result<(), String> {
    remap_span_source(&mut meta.span, remap)
}

fn remap_imports_spans(imports: &mut [Import], remap: &SourceIdRemap) -> Result<(), String> {
    for import in imports {
        remap_meta(&mut import.meta, remap)?;
    }
    Ok(())
}

fn remap_generic_params_spans(
    params: &mut [GenericParam],
    remap: &SourceIdRemap,
) -> Result<(), String> {
    for param in params {
        remap_meta(&mut param.meta, remap)?;
    }
    Ok(())
}

fn remap_span_source(span: &mut crate::span::Span, remap: &SourceIdRemap) -> Result<(), String> {
    span.source_id = remap
        .remap(span.source_id)
        .map_err(|error| format!("cannot remap AST span: {error}"))?;
    Ok(())
}

fn remap_item_spans(item: &mut Item, remap: &SourceIdRemap) -> Result<(), String> {
    match item {
        Item::Func(function) => remap_func_spans(function, remap),
        Item::Module(module) => {
            remap_meta(&mut module.meta, remap)?;
            remap_imports_spans(&mut module.imports, remap)?;
            for item in &mut module.items {
                remap_item_spans(item, remap)?;
            }
            Ok(())
        }
        Item::Type(type_def) => remap_type_def_spans(type_def, remap),
        Item::Cap(cap) => remap_meta(&mut cap.meta, remap),
        Item::Session(session) => {
            remap_meta(&mut session.meta, remap)?;
            remap_session_type_spans(&mut session.body, remap)
        }
        Item::Actor(actor) => {
            remap_meta(&mut actor.meta, remap)?;
            for field in &mut actor.fields {
                remap_meta(&mut field.meta, remap)?;
                remap_type_spans(&mut field.ty, remap)?;
                if let Some(init) = &mut field.init {
                    remap_expr_spans(init, remap)?;
                }
            }
            for method in &mut actor.methods {
                remap_func_spans(method, remap)?;
            }
            Ok(())
        }
        Item::Trait(trait_def) => {
            remap_meta(&mut trait_def.meta, remap)?;
            remap_generic_params_spans(&mut trait_def.generics, remap)?;
            for method in &mut trait_def.methods {
                remap_meta(&mut method.meta, remap)?;
                remap_generic_params_spans(&mut method.generics, remap)?;
                remap_params_spans(&mut method.params, remap)?;
                if let Some(ret) = &mut method.ret {
                    remap_type_spans(ret, remap)?;
                }
            }
            Ok(())
        }
        Item::Impl(impl_def) => {
            remap_meta(&mut impl_def.meta, remap)?;
            remap_generic_params_spans(&mut impl_def.generics, remap)?;
            for ty in &mut impl_def.trait_args {
                remap_type_spans(ty, remap)?;
            }
            for ty in &mut impl_def.type_args {
                remap_type_spans(ty, remap)?;
            }
            for method in &mut impl_def.methods {
                remap_func_spans(method, remap)?;
            }
            Ok(())
        }
        Item::ExternBlock(block) => {
            remap_meta(&mut block.meta, remap)?;
            for function in &mut block.funcs {
                remap_meta(&mut function.meta, remap)?;
                for param in &mut function.params {
                    remap_meta(&mut param.meta, remap)?;
                    remap_type_spans(&mut param.ty, remap)?;
                }
                if let Some(ret) = &mut function.ret {
                    remap_type_spans(ret, remap)?;
                }
                if let Some(requires) = &mut function.requires {
                    remap_expr_spans(requires, remap)?;
                }
                if let Some(ensures) = &mut function.ensures {
                    remap_expr_spans(ensures, remap)?;
                }
            }
            Ok(())
        }
        Item::Const {
            meta, ty, value, ..
        } => {
            remap_meta(meta, remap)?;
            if let Some(ty) = ty {
                remap_type_spans(ty, remap)?;
            }
            remap_expr_spans(value, remap)
        }
        Item::Flow(flow) => {
            remap_meta(&mut flow.meta, remap)?;
            remap_generic_params_spans(&mut flow.generics, remap)?;
            for annotation in &mut flow.annotations {
                remap_meta(&mut annotation.meta, remap)?;
            }
            for state in &mut flow.states {
                remap_meta(&mut state.meta, remap)?;
                if let Some(fields) = &mut state.payload {
                    remap_fields_spans(fields, remap)?;
                }
            }
            for transition in &mut flow.transitions {
                remap_meta(&mut transition.meta, remap)?;
                remap_params_spans(&mut transition.params, remap)?;
                if let Some(body) = &mut transition.body {
                    remap_block_spans(body, remap)?;
                }
            }
            Ok(())
        }
        Item::Protocol(protocol) => {
            remap_meta(&mut protocol.meta, remap)?;
            remap_generic_params_spans(&mut protocol.generics, remap)?;
            for state in &mut protocol.states {
                remap_meta(&mut state.meta, remap)?;
                if let Some(payload_type) = &mut state.payload_type {
                    remap_type_spans(payload_type, remap)?;
                }
            }
            for transition in &mut protocol.transitions {
                remap_meta(&mut transition.meta, remap)?;
            }
            Ok(())
        }
    }
}

fn remap_func_spans(function: &mut FuncDef, remap: &SourceIdRemap) -> Result<(), String> {
    remap_meta(&mut function.meta, remap)?;
    remap_generic_params_spans(&mut function.generics, remap)?;
    for clause in &mut function.where_clause {
        remap_meta(&mut clause.meta, remap)?;
    }
    remap_params_spans(&mut function.params, remap)?;
    if let Some(ret) = &mut function.ret {
        remap_type_spans(ret, remap)?;
    }
    remap_block_spans(&mut function.body, remap)
}

fn remap_params_spans(params: &mut [Param], remap: &SourceIdRemap) -> Result<(), String> {
    for param in params {
        remap_meta(&mut param.meta, remap)?;
        remap_type_spans(&mut param.ty, remap)?;
        if let Some(default) = &mut param.default_value {
            remap_expr_spans(default, remap)?;
        }
    }
    Ok(())
}

fn remap_block_spans(block: &mut Block, remap: &SourceIdRemap) -> Result<(), String> {
    for stmt in block {
        remap_stmt_spans(stmt, remap)?;
    }
    Ok(())
}

fn remap_stmt_spans(stmt: &mut Stmt, remap: &SourceIdRemap) -> Result<(), String> {
    match stmt {
        Stmt::Located { meta, stmt } => {
            remap_span_source(&mut meta.span, remap)?;
            remap_stmt_spans(stmt, remap)
        }
        Stmt::Let { pat, ty, init, .. } => {
            remap_pattern_spans(pat, remap)?;
            if let Some(ty) = ty {
                remap_type_spans(ty, remap)?;
            }
            if let Some(init) = init {
                remap_expr_spans(init, remap)?;
            }
            Ok(())
        }
        Stmt::Return(value) | Stmt::Break(value) => {
            if let Some(value) = value {
                remap_expr_spans(value, remap)?;
            }
            Ok(())
        }
        Stmt::Continue | Stmt::Ellipsis => Ok(()),
        Stmt::Expr(expr) | Stmt::Drop(expr) => remap_expr_spans(expr, remap),
        Stmt::If { cond, then_, else_ } => {
            remap_expr_spans(cond, remap)?;
            remap_block_spans(then_, remap)?;
            if let Some(else_) = else_ {
                remap_block_spans(else_, remap)?;
            }
            Ok(())
        }
        Stmt::While { cond, body } => {
            remap_expr_spans(cond, remap)?;
            remap_block_spans(body, remap)
        }
        Stmt::WhileLet { pat, init, body } => {
            remap_pattern_spans(pat, remap)?;
            remap_expr_spans(init, remap)?;
            remap_block_spans(body, remap)
        }
        Stmt::Loop(body)
        | Stmt::Block(body)
        | Stmt::Arena(body)
        | Stmt::Unsafe(body)
        | Stmt::OnFailure(body)
        | Stmt::Do(body)
        | Stmt::Parasteps(body) => remap_block_spans(body, remap),
        Stmt::For { iterable, body, .. } => {
            remap_expr_spans(iterable, remap)?;
            remap_block_spans(body, remap)
        }
        Stmt::Desc(_, span) | Stmt::Rule(_, span) => remap_span_source(span, remap),
        Stmt::Requires(expr, span) | Stmt::Ensures(expr, span) | Stmt::Invariant(expr, span) => {
            remap_span_source(span, remap)?;
            remap_expr_spans(expr, remap)
        }
        Stmt::Math(expressions) => {
            for expr in expressions {
                remap_expr_spans(expr, remap)?;
            }
            Ok(())
        }
        Stmt::Assign { target, value } => {
            remap_expr_spans(target, remap)?;
            remap_expr_spans(value, remap)
        }
        Stmt::SharedLet { ty, init, .. } => {
            if let Some(ty) = ty {
                remap_type_spans(ty, remap)?;
            }
            remap_expr_spans(init, remap)
        }
        Stmt::Delegate { expr, .. } => remap_expr_spans(expr, remap),
        Stmt::Pinned {
            expr,
            timeout,
            body,
            ..
        } => {
            remap_expr_spans(expr, remap)?;
            if let Some(timeout) = timeout {
                remap_expr_spans(timeout, remap)?;
            }
            remap_block_spans(body, remap)
        }
        Stmt::MmsBlock { span, .. } => remap_span_source(span, remap),
        Stmt::Func(function) => remap_func_spans(function, remap),
        Stmt::Alloc { body, .. } => remap_block_spans(body, remap),
    }
}

fn remap_pattern_spans(pattern: &mut Pattern, remap: &SourceIdRemap) -> Result<(), String> {
    remap_span_source(&mut pattern.meta.span, remap)?;
    match &mut pattern.kind {
        PatternKind::Constructor(_, fields) => {
            for (_, pattern) in fields {
                remap_pattern_spans(pattern, remap)?;
            }
        }
        PatternKind::Tuple(patterns) | PatternKind::Array(patterns) => {
            for pattern in patterns {
                remap_pattern_spans(pattern, remap)?;
            }
        }
        PatternKind::Slice(patterns, rest) => {
            for pattern in patterns {
                remap_pattern_spans(pattern, remap)?;
            }
            if let Some(rest) = rest {
                remap_pattern_spans(rest, remap)?;
            }
        }
        PatternKind::Wildcard | PatternKind::Variable(_) | PatternKind::Literal(_) => {}
    }
    Ok(())
}

fn remap_expr_spans(expr: &mut Expr, remap: &SourceIdRemap) -> Result<(), String> {
    match expr {
        Expr::Located { meta, expr } => {
            remap_span_source(&mut meta.span, remap)?;
            remap_expr_spans(expr, remap)
        }
        Expr::Literal(Lit::FString(parts)) => {
            for part in parts {
                if let FStringPart::Interp(expr) = part {
                    remap_expr_spans(expr, remap)?;
                }
            }
            Ok(())
        }
        Expr::Literal(_) | Expr::Ident(_) => Ok(()),
        Expr::TypeInfo(ty) => remap_type_spans(ty, remap),
        Expr::Binary(_, left, right) | Expr::Index(left, right) => {
            remap_expr_spans(left, remap)?;
            remap_expr_spans(right, remap)
        }
        Expr::Unary(_, value)
        | Expr::Try(value)
        | Expr::Spawn(value)
        | Expr::Await(value)
        | Expr::QuoteInterpolate(value)
        | Expr::TypeOf(value)
        | Expr::Old(value)
        | Expr::TupleIndex(value, _)
        | Expr::NamedArg(_, value) => remap_expr_spans(value, remap),
        Expr::Cast(value, ty) => {
            remap_expr_spans(value, remap)?;
            remap_type_spans(ty, remap)
        }
        Expr::Call(callee, args) => {
            remap_expr_spans(callee, remap)?;
            for arg in args {
                remap_expr_spans(arg, remap)?;
            }
            Ok(())
        }
        Expr::Field(value, _) | Expr::OptionalChain(value, _) => remap_expr_spans(value, remap),
        Expr::Tuple(values) | Expr::List(values) | Expr::SetLiteral(values) => {
            for value in values {
                remap_expr_spans(value, remap)?;
            }
            Ok(())
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            remap_expr_spans(expr, remap)?;
            remap_expr_spans(iter, remap)?;
            if let Some(guard) = guard {
                remap_expr_spans(guard, remap)?;
            }
            Ok(())
        }
        Expr::Match(value, arms) => {
            remap_expr_spans(value, remap)?;
            for arm in arms {
                remap_meta(&mut arm.meta, remap)?;
                remap_pattern_spans(&mut arm.pat, remap)?;
                if let Some(guard) = &mut arm.guard {
                    remap_expr_spans(guard, remap)?;
                }
                remap_expr_spans(&mut arm.body, remap)?;
            }
            Ok(())
        }
        Expr::Record { fields, .. } => {
            for field in fields {
                remap_meta(&mut field.meta, remap)?;
                remap_expr_spans(&mut field.value, remap)?;
            }
            Ok(())
        }
        Expr::Block(body) | Expr::Quote(body) | Expr::Comptime(body) | Expr::Arena(body) => {
            remap_block_spans(body, remap)
        }
        Expr::If { cond, then_, else_ } => {
            remap_expr_spans(cond, remap)?;
            remap_block_spans(then_, remap)?;
            if let Some(else_) = else_ {
                remap_block_spans(else_, remap)?;
            }
            Ok(())
        }
        Expr::Lambda {
            params, ret, body, ..
        } => {
            remap_params_spans(params, remap)?;
            if let Some(ret) = ret {
                remap_type_spans(ret, remap)?;
            }
            remap_block_spans(body, remap)
        }
        Expr::SliceExpr { target, start, end } => {
            remap_expr_spans(target, remap)?;
            if let Some(start) = start {
                remap_expr_spans(start, remap)?;
            }
            if let Some(end) = end {
                remap_expr_spans(end, remap)?;
            }
            Ok(())
        }
        Expr::Range { start, end } => {
            remap_expr_spans(start, remap)?;
            remap_expr_spans(end, remap)
        }
        Expr::Turbofish(_, type_args, args) => {
            for ty in type_args {
                remap_type_spans(ty, remap)?;
            }
            for arg in args {
                remap_expr_spans(arg, remap)?;
            }
            Ok(())
        }
        Expr::MapLiteral { entries } => {
            for (key, value) in entries {
                remap_expr_spans(key, remap)?;
                remap_expr_spans(value, remap)?;
            }
            Ok(())
        }
    }
}

fn remap_type_def_spans(type_def: &mut TypeDef, remap: &SourceIdRemap) -> Result<(), String> {
    remap_meta(&mut type_def.meta, remap)?;
    remap_generic_params_spans(&mut type_def.generics, remap)?;
    match &mut type_def.kind {
        TypeDefKind::Alias(ty) | TypeDefKind::Newtype(ty) => remap_type_spans(ty, remap),
        TypeDefKind::Record(fields) | TypeDefKind::Union(fields) => {
            remap_fields_spans(fields, remap)
        }
        TypeDefKind::Enum(variants) => {
            for variant in variants {
                remap_meta(&mut variant.meta, remap)?;
                if let Some(payload) = &mut variant.payload {
                    match payload {
                        VariantPayload::Tuple(types) => {
                            for ty in types {
                                remap_type_spans(ty, remap)?;
                            }
                        }
                        VariantPayload::Record(fields) => remap_fields_spans(fields, remap)?,
                    }
                }
            }
            Ok(())
        }
    }
}

fn remap_fields_spans(fields: &mut [Field], remap: &SourceIdRemap) -> Result<(), String> {
    for field in fields {
        remap_meta(&mut field.meta, remap)?;
        remap_type_spans(&mut field.ty, remap)?;
    }
    Ok(())
}

fn remap_session_type_spans(
    session: &mut SessionType,
    remap: &SourceIdRemap,
) -> Result<(), String> {
    match session {
        SessionType::Located { meta, session } => {
            remap_span_source(&mut meta.span, remap)?;
            remap_session_type_spans(session, remap)
        }
        SessionType::Send(ty, cont) | SessionType::Recv(ty, cont) => {
            remap_type_spans(ty, remap)?;
            remap_session_type_spans(cont, remap)
        }
        SessionType::Dual(inner) => remap_session_type_spans(inner, remap),
        SessionType::Name(_) | SessionType::End => Ok(()),
    }
}

fn remap_type_spans(ty: &mut Type, remap: &SourceIdRemap) -> Result<(), String> {
    match ty {
        Type::Located { meta, ty } => {
            remap_span_source(&mut meta.span, remap)?;
            remap_type_spans(ty, remap)
        }
        Type::Name(_, args) | Type::Tuple(args) => {
            for arg in args {
                remap_type_spans(arg, remap)?;
            }
            Ok(())
        }
        Type::Ref(_, inner)
        | Type::RefMut(_, inner)
        | Type::Option(inner)
        | Type::CBuffer(inner)
        | Type::Shared(inner)
        | Type::LocalShared(inner)
        | Type::Weak(inner)
        | Type::WeakLocal(inner)
        | Type::Newtype(_, inner)
        | Type::Array(inner, _)
        | Type::Slice(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::CShared(inner)
        | Type::CBorrow(inner)
        | Type::CBorrowMut(inner)
        | Type::ForAll(_, inner) => remap_type_spans(inner, remap),
        Type::Result(ok, err) => {
            remap_type_spans(ok, remap)?;
            remap_type_spans(err, remap)
        }
        Type::Func(params, ret) | Type::ExternFunc(params, ret) => {
            for param in params {
                remap_type_spans(param, remap)?;
            }
            remap_type_spans(ret, remap)
        }
        Type::Cap(_)
        | Type::Nothing
        | Type::Allocator
        | Type::ImplTrait(_)
        | Type::DynTrait(_)
        | Type::RawString
        | Type::Infer
        | Type::TypeVar(_) => Ok(()),
    }
}

// ── Entry point: load main file + all transitive imports ───────────────

pub fn flow_load_main(acc: Acc, path: &Path) -> Result<(Acc, LoadedModule), String> {
    flow_load_main_diagnostic(acc, path).map_err(|error| error.to_string())
}

fn flow_load_main_diagnostic(
    acc: Acc,
    path: &Path,
) -> Result<(Acc, LoadedModule), super::LoadDiagnosticError> {
    let canonical = path.canonicalize().map_err(|error| {
        super::LoadDiagnosticError::global(
            format!("cannot resolve path {}: {error}", path.display()),
            acc.source_registry.clone(),
        )
    })?;
    flow_load_file_diagnostic(acc, canonical)
}

/// Like `flow_load_main`, but use the provided AST for the main file instead of
/// reading it from disk (L-C1 unsaved buffer diagnostics).
pub fn flow_load_main_with_file(
    acc: Acc,
    path: &Path,
    file: File,
) -> Result<(Acc, LoadedModule), String> {
    flow_load_main_with_file_diagnostic(acc, path, file).map_err(|error| error.to_string())
}

pub(super) fn flow_load_main_with_file_diagnostic(
    mut acc: Acc,
    path: &Path,
    mut file: File,
) -> Result<(Acc, LoadedModule), super::LoadDiagnosticError> {
    let canonical = normalize_source_path(path);
    let incoming_source = if file.sources.is_empty() {
        None
    } else {
        let expected_key = acc.source_key(&canonical);
        Some(
            file.sources
                .id_for_disk_path(&canonical)
                .or_else(|| file.sources.id_for_uri(&source_file_uri(&canonical)))
                .or_else(|| file.sources.id_for_key(&expected_key))
                .ok_or_else(|| {
                    super::LoadDiagnosticError::global(
                        format!(
                            "in-memory AST source registry does not identify main file '{}' by disk path, URI, or SourceKey; refusing to guess from registry order",
                            canonical.display()
                        ),
                        acc.source_registry.clone(),
                    )
                })?,
        )
    };

    let source_id = if let Some(incoming_source) = incoming_source {
        let remap = acc
            .source_registry
            .merge_from(&file.sources)
            .map_err(|error| {
                super::LoadDiagnosticError::global(
                    format!("cannot merge in-memory source registry: {error}"),
                    acc.source_registry.clone(),
                )
            })?;
        remap_file_spans(&mut file, &remap).map_err(|error| {
            super::LoadDiagnosticError::global(error, acc.source_registry.clone())
        })?;
        file.sources = acc.source_registry.clone();
        remap.remap(incoming_source).map_err(|error| {
            super::LoadDiagnosticError::global(
                format!("cannot remap in-memory main source: {error}"),
                acc.source_registry.clone(),
            )
        })?
    } else {
        // An empty registry is valid only when every reachable span is
        // explicitly unregistered. Validate that invariant before attaching
        // the AST to its newly registered single memory source.
        remap_file_spans(&mut file, &SourceIdRemap::default()).map_err(|error| {
            super::LoadDiagnosticError::global(error, acc.source_registry.clone())
        })?;
        let sources_before_registration = acc.source_registry.clone();
        let (new_acc, source_id) = acc
            .source_id_for(&canonical, SourceTextOrigin::Memory)
            .map_err(|error| {
                super::LoadDiagnosticError::global(error, sources_before_registration)
            })?;
        acc = new_acc;
        remap_file_spans(&mut file, &SourceIdRemap::attach_unknown_to(source_id)).map_err(
            |error| super::LoadDiagnosticError::global(error, acc.source_registry.clone()),
        )?;
        file.sources = acc.source_registry.clone();
        source_id
    };
    acc.source_ids.insert(canonical.clone(), source_id);
    let module_name = acc.module_key(&canonical);
    let loaded = LoadedModule {
        path: canonical.clone(),
        file: file.clone(),
    };
    acc.modules.insert(module_name, loaded.clone());
    acc.loaded.insert(canonical.clone(), loaded.clone());
    // Load transitive imports from the in-memory main file.
    let imports = file.imports.clone();
    for import in &imports {
        let import_path = resolve_import_path(&canonical, &import.path, &acc).map_err(|error| {
            super::LoadDiagnosticError::at(error, import.meta.span, acc.source_registry.clone())
        })?;
        let (new_acc, _) = flow_load_file_diagnostic(acc, import_path)?;
        acc = new_acc;
    }
    let registry = acc.source_registry.clone();
    for module in acc.loaded.values_mut() {
        module.file.sources = registry.clone();
    }
    for module in acc.modules.values_mut() {
        module.file.sources = registry.clone();
    }
    let loaded = acc.loaded.get(&canonical).cloned().unwrap_or(loaded);
    Ok((acc, loaded))
}

/// Load a file (from cache or fresh) and all its transitive imports.
pub fn flow_load_file(acc: Acc, path: PathBuf) -> Result<(Acc, LoadedModule), String> {
    flow_load_file_diagnostic(acc, path).map_err(|error| error.to_string())
}

fn flow_load_file_diagnostic(
    mut acc: Acc,
    path: PathBuf,
) -> Result<(Acc, LoadedModule), super::LoadDiagnosticError> {
    let path = normalize_source_path(&path);
    // Check cache first
    if acc.loaded.contains_key(&path) {
        let m = acc.loaded[&path].clone();
        return Ok((acc, m));
    }

    // Worklist as DFS stack. Each entry: (file_path, ancestor_chain).
    // ancestor_chain is the set of paths leading to this file (for cycle detection).
    let mut worklist: Vec<(PathBuf, Vec<PathBuf>)> = vec![(path.clone(), vec![path.clone()])];
    // Files that have been parsed (but whose imports may not be fully resolved yet).
    let mut parsed: HashMap<PathBuf, File> = HashMap::new();
    // Import edges for post-processing.
    let mut import_edges: Vec<ImportEdge> = Vec::new();

    while let Some((file_path, ancestors)) = worklist.pop() {
        if parsed.contains_key(&file_path) || acc.loaded.contains_key(&file_path) {
            continue;
        }

        let sources_before_registration = acc.source_registry.clone();
        let (new_acc, source_id) = acc
            .source_id_for(&file_path, SourceTextOrigin::Disk)
            .map_err(|error| {
                super::LoadDiagnosticError::global(error, sources_before_registration)
            })?;
        acc = new_acc;

        // CL-H1: reject oversized module sources before parse. Register the
        // path first so even read/lex failures route to the dependency URI.
        let source_anchor = crate::span::Span::UNKNOWN.with_source(source_id);
        let source = crate::path_safety::read_source_capped(&file_path).map_err(|error| {
            super::LoadDiagnosticError::at(
                format!("cannot read {}: {error}", file_path.display()),
                source_anchor,
                acc.source_registry.clone(),
            )
        })?;

        let tokens = lexer::Lexer::new(&source).tokenize().map_err(|error| {
            let (line, col) = error.position();
            super::LoadDiagnosticError::at(
                format!("lexer error in {}: {error}", file_path.display()),
                crate::span::Span::single(line, col).with_source(source_id),
                acc.source_registry.clone(),
            )
        })?;
        let file = parser::Parser::new_with_source_registry(
            tokens,
            source_id,
            acc.source_registry.clone(),
        )
        .parse_file()
        .map_err(|error| super::LoadDiagnosticError {
            diagnostic: Box::new(error.to_diagnostic()),
            sources: acc.source_registry.clone(),
        })?;

        // Resolve imports for this file
        let mut resolved: Vec<ResolvedImport> = Vec::new();
        for import in &file.imports {
            let import_path = normalize_source_path(
                &resolve_import_path(&file_path, &import.path, &acc).map_err(|error| {
                    // CL-H2: when a dependency cannot be resolved, register
                    // its candidate path as a source identity so downstream
                    // diagnostics retain an honest `SourceId` for the
                    // unresolved dependency. The text range is UNKNOWN because
                    // no readable source exists; the identity is the canonical
                    // path the loader would have used (base/import_path.mimi).
                    let candidate = normalize_source_path(
                        &file_path
                            .parent()
                            .unwrap_or(&acc.base_dir)
                            .join(import.path.iter().collect::<std::path::PathBuf>())
                            .with_extension("mimi"),
                    );
                    match acc
                        .clone()
                        .source_id_for(&candidate, SourceTextOrigin::Disk)
                    {
                        Ok((registered_acc, source_id)) => super::LoadDiagnosticError::at(
                            error,
                            crate::span::Span::UNKNOWN.with_source(source_id),
                            registered_acc.source_registry.clone(),
                        ),
                        // If registration fails, fall back to the importing
                        // file's span rather than masking the identity loss.
                        Err(_) => super::LoadDiagnosticError::at(
                            error,
                            import.meta.span,
                            acc.source_registry.clone(),
                        ),
                    }
                })?,
            );
            resolved.push((import.path.clone(), import_path.clone()));

            // Cycle check: is the import target in the current ancestor chain?
            if ancestors.contains(&import_path) {
                return Err(super::LoadDiagnosticError::at(
                    format!(
                        "circular dependency detected: {} imports itself",
                        import_path.display()
                    ),
                    import.meta.span,
                    acc.source_registry.clone(),
                ));
            }

            // Add to worklist if not already processed
            if !parsed.contains_key(&import_path) && !acc.loaded.contains_key(&import_path) {
                let mut child_ancestors = ancestors.clone();
                child_ancestors.push(import_path.clone());
                worklist.push((import_path, child_ancestors));
            }
        }
        import_edges.push((file_path.clone(), resolved));
        parsed.insert(file_path, file);
    }

    // Every parsed file shares the final session registry, including sources
    // discovered after that file itself was parsed.
    let registry = acc.source_registry.clone();
    for file in parsed.values_mut() {
        file.sources = registry.clone();
    }

    // Register all parsed files as LoadedModules
    for (file_path, file) in &parsed {
        let module_name = acc.module_key(file_path);
        let loaded = LoadedModule {
            path: file_path.clone(),
            file: file.clone(),
        };
        acc.modules.insert(module_name, loaded.clone());
        acc.loaded.insert(file_path.clone(), loaded);
    }

    // Process import edges: ensure each import's module is registered in modules map
    for (_parent_path, edges) in &import_edges {
        for (_, import_file_path) in edges {
            let dep_key = acc.module_key(import_file_path);
            if !acc.modules.contains_key(&dep_key) {
                if let Some(dep) = acc.loaded.get(import_file_path) {
                    acc.modules.insert(dep_key, dep.clone());
                }
            }
        }
    }

    // Return the main module
    let main_module = acc.loaded.get(&path).cloned().ok_or_else(|| {
        super::LoadDiagnosticError::global("main file was not loaded", acc.source_registry.clone())
    })?;

    Ok((acc, main_module))
}

// ── Import resolution (pure function, no &mut self) ────────────────────

fn resolve_import_path(from: &Path, import_path: &[String], acc: &Acc) -> Result<PathBuf, String> {
    for segment in import_path {
        if segment == ".." || segment.contains('/') || segment.contains('\\') {
            return Err(format!(
                "import path '{}' contains invalid segment '{}'",
                import_path.join("::"),
                segment
            ));
        }
    }
    let base = from.parent().unwrap_or(&acc.base_dir);
    let relative: PathBuf = import_path.iter().collect();

    let try_paths = |paths: &[PathBuf]| -> Option<PathBuf> {
        for p in paths {
            if p.exists() {
                return Some(p.clone());
            }
        }
        None
    };

    // 1. Relative to importing file
    let file_path = base.join(&relative).with_extension("mimi");
    if file_path.exists() {
        return Ok(file_path);
    }

    // 2. Relative to base_dir
    let base_path = acc.base_dir.join(&relative).with_extension("mimi");
    if base_path.exists() {
        return Ok(base_path);
    }

    // 3. Dependency paths
    if let Some(first) = import_path.first() {
        if let Some(dep_dir) = acc.dep_paths.get(first) {
            let dep_relative: PathBuf = import_path.iter().skip(1).collect();
            let dep_path = dep_dir.join(&dep_relative).with_extension("mimi");
            if dep_path.exists() {
                return Ok(dep_path);
            }
            let dep_root = dep_dir.with_extension("mimi");
            if dep_root.exists() && import_path.len() == 1 {
                return Ok(dep_root);
            }
            if import_path.len() == 1 && dep_dir.is_dir() {
                if let Ok(Some((manifest_dir, manifest))) = manifest::Manifest::find(dep_dir) {
                    let entry_path = manifest.entry_path(&manifest_dir);
                    if entry_path.exists() {
                        return Ok(entry_path);
                    }
                }
            }
        }
    }

    // 4. .mimi/deps/ — P-H7: only packages declared in lockfile/manifest deps.
    let deps_dir = acc.base_dir.join(".mimi").join("deps");
    if deps_dir.exists() {
        if let Some(first) = import_path.first() {
            let declared =
                acc.dep_paths.contains_key(first) || acc.lock_entries.contains_key(first);
            if !declared {
                // Fall through — do not load undeclared cached packages.
            } else {
                let dep_root = deps_dir.join(first);
                if dep_root.exists() {
                    let dep_relative: PathBuf = import_path.iter().skip(1).collect();
                    if let Some(found) = try_paths(&[
                        dep_root.join(&dep_relative).with_extension("mimi"),
                        dep_root.with_extension("mimi"),
                    ]) {
                        if import_path.len() == 1 || found != dep_root.with_extension("mimi") {
                            return Ok(found);
                        }
                    }
                }
            }
        }
    }

    // 5. stdlib
    if let Some(std_dir) = super::stdlib_dir() {
        // With "std" prefix
        if import_path.first().map(|s| s == "std").unwrap_or(false) {
            if let Some(found) = try_paths(&[
                std_dir.join(&relative).with_extension("mimi"),
                std_dir
                    .join(import_path.iter().skip(1).collect::<PathBuf>())
                    .with_extension("mimi"),
            ]) {
                return Ok(found);
            }
        }
        // Without prefix
        if let Some(found) = try_paths(&[std_dir.join(&relative).with_extension("mimi")]) {
            return Ok(found);
        }
    }

    // 6. Selective import (use foo::bar → resolve foo.mimi)
    if import_path.len() >= 2 {
        let prefix: PathBuf = import_path[..import_path.len() - 1].iter().collect();
        // Relative to parent
        if let Some(found) = try_paths(&[
            base.join(&prefix).with_extension("mimi"),
            acc.base_dir.join(&prefix).with_extension("mimi"),
        ]) {
            return Ok(found);
        }
        // Dependency paths for selective
        if let Some(first) = import_path.first() {
            if let Some(dep_dir) = acc.dep_paths.get(first) {
                let dep_prefix: PathBuf = import_path[1..import_path.len() - 1].iter().collect();
                let dep_path = dep_dir.join(&dep_prefix).with_extension("mimi");
                if dep_path.exists() {
                    return Ok(dep_path);
                }
                if dep_prefix.as_os_str().is_empty() && dep_dir.is_dir() {
                    if let Ok(Some((manifest_dir, manifest))) = manifest::Manifest::find(dep_dir) {
                        let entry_path = manifest.entry_path(&manifest_dir);
                        if entry_path.exists() {
                            return Ok(entry_path);
                        }
                    }
                }
            }
        }
        // stdlib for selective
        if let Some(std_dir) = super::stdlib_dir() {
            if import_path.first().map(|s| s == "std").unwrap_or(false) {
                let sub_prefix: PathBuf = import_path[1..import_path.len() - 1].iter().collect();
                if let Some(found) = try_paths(&[std_dir.join(&sub_prefix).with_extension("mimi")])
                {
                    return Ok(found);
                }
            }
            if let Some(found) = try_paths(&[std_dir.join(&prefix).with_extension("mimi")]) {
                return Ok(found);
            }
        }
    }

    Err(format!(
        "cannot find module '{}' (looked in {}, {}, and stdlib)",
        import_path.join("::"),
        base.display(),
        acc.base_dir.display()
    ))
}

// ── merge_all — same logic as legacy, operates on &Acc ─────────────────

pub fn flow_merge_all(modules: &HashMap<String, LoadedModule>) -> Result<File, String> {
    let mut all_items = Vec::new();
    let mut seen_imports = HashSet::new();
    let mut all_imports = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut sources = SourceRegistry::default();

    // HashMap iteration order and each module's dense SourceId allocation are
    // intentionally irrelevant. Normalize every AST into one registry before
    // cloning any source-aware node into the merged file.
    let mut ordered_modules: Vec<_> = modules.iter().collect();
    ordered_modules.sort_by(|left, right| left.0.cmp(right.0));
    let mut normalized = Vec::with_capacity(ordered_modules.len());
    for (_, module) in ordered_modules {
        let mut file = module.file.clone();
        remap_file_into_registry(&mut file, &mut sources)?;
        normalized.push((module, file));
    }

    for (module, file) in normalized {
        // P-H6: dependency modules only contribute `pub` items.
        let is_dep = {
            let p = module.path.to_string_lossy();
            p.contains("/std/")
                || p.contains("\\std\\")
                || p.contains("/.mimi/deps/")
                || p.contains("\\.mimi\\deps\\")
        };
        for item in &file.items {
            if is_dep && !item_is_pub(item) {
                continue;
            }
            if let Some(name) = item_name(item) {
                if !seen_names.insert(name.to_string()) {
                    let dup_modules: Vec<String> = modules
                        .values()
                        .filter(|m| m.file.items.iter().any(|i| item_name(i) == Some(name)))
                        .map(|m| m.path.display().to_string())
                        .collect();
                    return Err(format!(
                        "duplicate item '{}' found in modules: {}",
                        name,
                        dup_modules.join(", ")
                    ));
                }
            }
            all_items.push(item.clone());
        }
        for imp in &file.imports {
            if seen_imports.insert(imp.path.clone()) {
                all_imports.push(imp.clone());
            }
        }
    }

    Ok(File {
        sources,
        imports: all_imports,
        items: all_items,
        implicit_single: false,
    })
}

fn item_is_pub(item: &Item) -> bool {
    match item {
        Item::Func(f) => f.pub_,
        Item::Type(td) => td.pub_,
        Item::Actor(a) => a.pub_,
        Item::Const { pub_, .. } => *pub_,
        Item::Flow(f) => f.pub_,
        Item::Session(s) => s.pub_,
        // Traits/impls/modules/extern/protocol/cap: treat as public API surface.
        Item::Module(_)
        | Item::Trait(_)
        | Item::Impl(_)
        | Item::ExternBlock(_)
        | Item::Protocol(_)
        | Item::Cap(_) => true,
    }
}

fn item_name(item: &Item) -> Option<&str> {
    match item {
        Item::Func(f) => Some(&f.name),
        Item::Module(m) => Some(&m.name),
        Item::Type(t) => Some(&t.name),
        Item::Actor(a) => Some(&a.name),
        Item::Cap(c) => Some(&c.name),
        Item::Trait(t) => Some(&t.name),
        Item::Impl(i) => Some(i.type_name.as_str()),
        Item::ExternBlock(_) => None,
        Item::Const { name, .. } => Some(name),
        Item::Flow(f) => Some(&f.name),
        Item::Protocol(p) => Some(&p.name),
        Item::Session(s) => Some(&s.name),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::legacy::LegacyLoader;
    use std::fs;
    use std::path::Path;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mimi_test_flow_loader_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn cleanup(dir: &PathBuf) {
        let _ = fs::remove_dir_all(dir);
    }

    fn parse_registered(source: &str, source_id: SourceId, sources: SourceRegistry) -> File {
        let tokens = lexer::Lexer::new(source)
            .tokenize()
            .expect("tokenize test source");
        parser::Parser::new_with_source_registry(tokens, source_id, sources)
            .parse_file()
            .expect("parse test source")
    }

    fn requires_source(file: &File, function_name: &str) -> SourceId {
        file.items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == function_name => function
                    .body
                    .iter()
                    .find_map(|stmt| match stmt.unlocated() {
                        Stmt::Requires(_, span) => Some(span.source_id),
                        _ => None,
                    }),
                _ => None,
            })
            .expect("function requires span")
    }

    fn source_keys(registry: &SourceRegistry) -> Vec<String> {
        let mut keys: Vec<_> = registry
            .records()
            .iter()
            .map(|record| record.key.as_str().to_string())
            .collect();
        keys.sort();
        keys
    }

    fn collect_type_source_ids(ty: &Type, source_ids: &mut Vec<SourceId>) {
        match ty {
            Type::Located { meta, ty } => {
                source_ids.push(meta.span.source_id);
                collect_type_source_ids(ty, source_ids);
            }
            Type::Name(_, args) | Type::Tuple(args) => {
                for arg in args {
                    collect_type_source_ids(arg, source_ids);
                }
            }
            Type::Ref(_, inner)
            | Type::RefMut(_, inner)
            | Type::Option(inner)
            | Type::CBuffer(inner)
            | Type::Shared(inner)
            | Type::LocalShared(inner)
            | Type::Weak(inner)
            | Type::WeakLocal(inner)
            | Type::Newtype(_, inner)
            | Type::Array(inner, _)
            | Type::Slice(inner)
            | Type::RawPtr(inner)
            | Type::RawPtrMut(inner)
            | Type::CShared(inner)
            | Type::CBorrow(inner)
            | Type::CBorrowMut(inner)
            | Type::ForAll(_, inner) => collect_type_source_ids(inner, source_ids),
            Type::Result(ok, err) => {
                collect_type_source_ids(ok, source_ids);
                collect_type_source_ids(err, source_ids);
            }
            Type::Func(params, ret) | Type::ExternFunc(params, ret) => {
                for param in params {
                    collect_type_source_ids(param, source_ids);
                }
                collect_type_source_ids(ret, source_ids);
            }
            Type::Cap(_)
            | Type::Nothing
            | Type::Allocator
            | Type::ImplTrait(_)
            | Type::DynTrait(_)
            | Type::RawString
            | Type::Infer
            | Type::TypeVar(_) => {}
        }
    }

    /// Compare flow loader result with legacy loader result for equivalence.
    fn assert_load_equivalent(dir: &Path, path: &Path) {
        let semantic_debug = |file: &File| {
            let mut file = file.clone();
            // This legacy oracle compares parser/module semantics, not source
            // registry allocation. Erase every reachable source ID as well as
            // the table itself; clearing only `File::sources` would leave the
            // source-aware Stmt/Expr/Pattern metadata unequal.
            let erase = SourceIdRemap::erase_for_semantic_comparison(&file.sources);
            remap_file_spans(&mut file, &erase).expect("erase source identity for oracle");
            file.sources = SourceRegistry::default();
            format!("{file:?}")
        };
        // Flow version
        let acc = Acc::new(dir.to_path_buf());
        let (acc, flow_main) = flow_load_main(acc, path).expect("flow_load_main failed");
        let flow_modules = acc.modules.clone();

        // Legacy version
        let mut legacy = LegacyLoader::new(dir.to_path_buf());
        let legacy_main = legacy.load_main(path).expect("legacy load_main failed");
        let legacy_modules = legacy.modules.clone();

        // Compare main module
        assert_eq!(
            semantic_debug(&flow_main.file),
            semantic_debug(&legacy_main.file),
            "main module AST mismatch"
        );

        // Compare module set
        let flow_keys: HashSet<_> = flow_modules.keys().collect();
        let legacy_keys: HashSet<_> = legacy_modules.keys().collect();
        assert_eq!(flow_keys, legacy_keys, "module key set mismatch");
        for key in flow_keys {
            assert_eq!(
                semantic_debug(&flow_modules[key].file),
                semantic_debug(&legacy_modules[key].file),
                "file mismatch for module '{}'",
                key
            );
        }
    }

    // ── Single file ──────────────────────────────────────────────────

    #[test]
    fn test_flow_loader_single_file() {
        let dir = temp_dir("single");
        let file_path = dir.join("main.mimi");
        fs::write(&file_path, "func main() -> i32 { 42 }").unwrap();
        assert_load_equivalent(&dir, &file_path);
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_single_file_with_import() {
        let dir = temp_dir("single_import");
        fs::write(
            dir.join("main.mimi"),
            "use lib;\nfunc main() -> i32 { lib::helper() }",
        )
        .unwrap();
        fs::write(dir.join("lib.mimi"), "pub func helper() -> i32 { 7 }").unwrap();
        assert_load_equivalent(&dir, &dir.join("main.mimi"));
        cleanup(&dir);
    }

    #[test]
    fn loaded_files_receive_distinct_stable_source_ids() {
        let dir = temp_dir("source_ids");
        let main_path = dir.join("main.mimi");
        let lib_path = dir.join("lib.mimi");
        fs::write(
            &main_path,
            "use lib;\nfunc main() -> i32 { requires: true\n return lib::helper() }",
        )
        .unwrap();
        fs::write(
            &lib_path,
            "pub func helper() -> i32 { requires: true\n return 7 }",
        )
        .unwrap();

        let acc = Acc::new(dir.clone());
        let (acc, main) = flow_load_main(acc, &main_path).expect("load");
        let lib = acc
            .loaded
            .get(&lib_path.canonicalize().expect("canonical lib"))
            .expect("loaded lib");
        let requires_source = |file: &File| {
            file.items
                .iter()
                .find_map(|item| match item {
                    Item::Func(function) => {
                        function
                            .body
                            .iter()
                            .find_map(|stmt| match stmt.unlocated() {
                                Stmt::Requires(_, span) => Some(span.source_id),
                                _ => None,
                            })
                    }
                    _ => None,
                })
                .expect("requires span")
        };
        let main_source = requires_source(&main.file);
        let lib_source = requires_source(&lib.file);
        assert!(main_source.is_known());
        assert!(lib_source.is_known());
        assert_ne!(main_source, lib_source);

        let (acc, cached) = flow_load_main(acc, &main_path).expect("cached load");
        assert_eq!(requires_source(&cached.file), main_source);
        assert_eq!(acc.source_ids.len(), 2);

        let (independent_acc, independently_loaded_lib) =
            flow_load_main(Acc::new(dir.clone()), &lib_path).expect("independent lib load");
        let independent_source = requires_source(&independently_loaded_lib.file);
        assert_eq!(
            independent_acc
                .source_registry
                .key(independent_source)
                .map(SourceKey::as_str),
            acc.source_registry.key(lib_source).map(SourceKey::as_str)
        );
        cleanup(&dir);
    }

    #[test]
    fn workspace_source_key_is_independent_of_loader_base_dir() {
        let dir = temp_dir("workspace_source_key");
        let src_dir = dir.join("src");
        fs::create_dir_all(&src_dir).expect("src dir");
        fs::write(
            dir.join("mimi.toml"),
            "[package]\nname = \"stable-app\"\nversion = \"0.1.0\"\nentry = \"src/main.mimi\"\n",
        )
        .expect("manifest");
        let main_path = src_dir.join("main.mimi");
        fs::write(
            &main_path,
            "func main() -> i32 { requires: true\n return 1 }",
        )
        .expect("main source");

        let (root_acc, root_main) =
            flow_load_main(Acc::new(dir.clone()), &main_path).expect("root load");
        let root_id = requires_source(&root_main.file, "main");
        let root_key = root_acc.source_registry.key(root_id).expect("root key");

        let (nested_acc, nested_main) =
            flow_load_main(Acc::new(src_dir), &main_path).expect("nested load");
        let nested_id = requires_source(&nested_main.file, "main");
        let nested_key = nested_acc
            .source_registry
            .key(nested_id)
            .expect("nested key");

        assert_eq!(root_key, nested_key);
        assert_eq!(root_key.as_str(), "workspace:src/main.mimi");
        cleanup(&dir);
    }

    #[test]
    fn manifestless_source_key_uses_canonical_identity_not_base_dir() {
        let dir = temp_dir("manifestless_source_key");
        let nested = dir.join("nested");
        fs::create_dir_all(&nested).expect("nested dir");
        let source = nested.join("main.mimi");
        fs::write(&source, "func main() -> i32 { 1 }").expect("source");

        let root_key = Acc::new(dir.clone()).source_key(&source);
        let nested_key = Acc::new(nested).source_key(&source);
        assert_eq!(root_key, nested_key);
        assert!(root_key.as_str().starts_with("external:"));
        cleanup(&dir);
    }

    #[test]
    fn source_keys_are_independent_of_import_discovery_order() {
        let dir = temp_dir("source_import_order");
        let main_path = dir.join("main.mimi");
        fs::write(dir.join("a.mimi"), "pub func a() -> i32 { 1 }").expect("a");
        fs::write(dir.join("b.mimi"), "pub func b() -> i32 { 2 }").expect("b");
        fs::write(
            &main_path,
            "use a;\nuse b;\nfunc main() -> i32 { a::a() + b::b() }",
        )
        .expect("main a-b");
        let (first, _) = flow_load_main(Acc::new(dir.clone()), &main_path).expect("first load");

        fs::write(
            &main_path,
            "use b;\nuse a;\nfunc main() -> i32 { a::a() + b::b() }",
        )
        .expect("main b-a");
        let (second, _) = flow_load_main(Acc::new(dir.clone()), &main_path).expect("second load");

        assert_eq!(
            source_keys(&first.source_registry),
            source_keys(&second.source_registry)
        );
        cleanup(&dir);
    }

    #[test]
    fn dependency_and_stdlib_sources_use_package_identities() {
        let dir = temp_dir("package_source_keys");
        let dep = dir.join("sibling-dep");
        fs::create_dir_all(&dep).expect("dependency dir");
        fs::write(
            dep.join("mimi.toml"),
            "[package]\nname = \"canonical-dep\"\nversion = \"2.4.0\"\nentry = \"lib.mimi\"\n",
        )
        .expect("dependency manifest");
        let dep_file = dep.join("lib.mimi");
        fs::write(&dep_file, "pub func value() -> i32 { 1 }").expect("dependency source");
        fs::write(
            dir.join("mimi.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"dep\"\npath = \"sibling-dep\"\n",
        )
        .expect("manifest");
        let acc = Acc::new(dir.clone());
        assert_eq!(
            acc.source_key(&dep_file).as_str(),
            "package:canonical-dep@2.4.0:lib.mimi"
        );

        let std_dir = super::super::stdlib_dir().expect("stdlib directory");
        assert_eq!(
            acc.source_key(&std_dir.join("prelude.mimi")).as_str(),
            "stdlib:prelude.mimi"
        );
        cleanup(&dir);
    }

    #[test]
    fn in_memory_main_without_registry_attaches_registered_source_to_exact_spans() {
        let dir = temp_dir("memory_attach_unknown_source");
        let main_path = dir.join("main.mimi");
        fs::write(&main_path, "// disk placeholder").expect("disk placeholder");
        let source = "type Box<T> { value: T }\nfunc main(input: Box<i32>) -> i32 { return 1 }";
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex in-memory source");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse in-memory source");
        assert!(file.sources.is_empty());
        let before = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Type(type_def) if type_def.name == "Box" => Some(type_def),
                _ => None,
            })
            .expect("expected type declaration");
        assert_eq!(before.meta.span.source_id, SourceId::UNKNOWN);
        assert!(before.meta.span.start_line > 0);

        let (acc, loaded) = flow_load_main_with_file(Acc::new(dir.clone()), &main_path, file)
            .expect("attach in-memory source");
        let source_id = loaded
            .file
            .sources
            .id_for_disk_path(&main_path)
            .expect("registered in-memory main source");
        assert!(source_id.is_known());
        assert!(acc.source_registry.record(source_id).is_some());

        let type_def = loaded
            .file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Type(type_def) if type_def.name == "Box" => Some(type_def),
                _ => None,
            })
            .expect("expected type declaration");
        assert_eq!(type_def.meta.span.source_id, source_id);
        assert_eq!(type_def.generics[0].meta.span.source_id, source_id);
        let TypeDefKind::Record(fields) = &type_def.kind else {
            panic!("expected record declaration");
        };
        assert_eq!(fields[0].meta.span.source_id, source_id);
        assert_eq!(fields[0].ty.meta().unwrap().span.source_id, source_id);

        let function = loaded
            .file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("expected function declaration");
        assert_eq!(function.meta.span.source_id, source_id);
        assert_eq!(function.params[0].meta.span.source_id, source_id);
        assert_eq!(
            function.params[0].ty.meta().unwrap().span.source_id,
            source_id
        );
        assert_eq!(function.body[0].meta().unwrap().span.source_id, source_id);
        cleanup(&dir);
    }

    #[test]
    fn in_memory_main_registry_is_unioned_and_all_ast_spans_are_remapped() {
        let dir = temp_dir("memory_registry_union");
        let main_path = dir.join("main.mimi");
        fs::write(&main_path, "// disk placeholder").expect("disk placeholder");
        let canonical_main = main_path.canonicalize().expect("canonical main");

        let mut incoming = SourceRegistry::default();
        incoming
            .register(
                SourceRecord::new(
                    SourceKey::new("memory:decoy").expect("decoy key"),
                    SourceTextOrigin::Memory,
                )
                .with_uri("memory:///decoy"),
            )
            .expect("decoy first");
        let incoming_main = incoming
            .register(
                SourceRecord::new(
                    SourceKey::new("workspace:main.mimi").expect("main key"),
                    SourceTextOrigin::Memory,
                )
                .with_uri(source_file_uri(&canonical_main))
                .with_disk_path(canonical_main.clone()),
            )
            .expect("main second");
        assert_eq!(incoming_main, SourceId::new(2));
        let source = "type Box<T> { value: T }\nfunc main(input: i32) -> i32 { requires: true\n let value = input\n return value }";
        let file = parse_registered(source, incoming_main, incoming);

        let mut existing = SourceRegistry::default();
        existing
            .register_key("session:already-loaded", SourceTextOrigin::Generated)
            .expect("existing source");
        let acc = Acc::new(dir.clone()).with_source_registry(HashMap::new(), existing);
        let (acc, loaded) = flow_load_main_with_file(acc, &main_path, file).expect("memory load");

        let remapped_main = requires_source(&loaded.file, "main");
        assert_ne!(remapped_main, incoming_main);
        assert_eq!(
            acc.source_registry
                .key(remapped_main)
                .map(SourceKey::as_str),
            Some("workspace:main.mimi")
        );
        let function = loaded
            .file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main function");
        assert_eq!(function.meta.span.source_id, remapped_main);
        assert_eq!(function.params[0].meta.span.source_id, remapped_main);
        assert_eq!(
            function
                .ret
                .as_ref()
                .unwrap()
                .meta()
                .unwrap()
                .span
                .source_id,
            remapped_main
        );
        let type_def = loaded
            .file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Type(type_def) if type_def.name == "Box" => Some(type_def),
                _ => None,
            })
            .expect("Box type");
        assert_eq!(type_def.meta.span.source_id, remapped_main);
        assert_eq!(type_def.generics[0].meta.span.source_id, remapped_main);
        let TypeDefKind::Record(fields) = &type_def.kind else {
            panic!("Box record");
        };
        assert_eq!(fields[0].meta.span.source_id, remapped_main);
        assert_eq!(fields[0].ty.meta().unwrap().span.source_id, remapped_main);
        for stmt in &function.body {
            assert_eq!(
                stmt.meta().expect("statement metadata").span.source_id,
                remapped_main
            );
            if let Stmt::Let {
                pat,
                init: Some(init),
                ..
            } = stmt.unlocated()
            {
                assert_eq!(pat.meta.span.source_id, remapped_main);
                assert_eq!(
                    init.meta().expect("initializer metadata").span.source_id,
                    remapped_main
                );
            }
        }
        cleanup(&dir);
    }

    #[test]
    fn in_memory_main_with_ambiguous_registry_fails_closed() {
        let dir = temp_dir("memory_registry_ambiguous");
        let main_path = dir.join("main.mimi");
        fs::write(&main_path, "// disk placeholder").expect("disk placeholder");
        let mut incoming = SourceRegistry::default();
        let source_id = incoming
            .register(
                SourceRecord::new(
                    SourceKey::new("memory:unrelated").expect("key"),
                    SourceTextOrigin::Memory,
                )
                .with_uri("memory:///unrelated"),
            )
            .expect("unrelated source");
        let file = parse_registered("func main() -> i32 { 1 }", source_id, incoming);
        let error = match flow_load_main_with_file(Acc::new(dir.clone()), &main_path, file) {
            Err(error) => error,
            Ok(_) => panic!("loader must not select records().first()"),
        };
        assert!(error.contains("refusing to guess"), "{error}");
        cleanup(&dir);
    }

    #[test]
    fn in_memory_ast_with_unregistered_known_span_fails_closed() {
        let dir = temp_dir("memory_registry_missing_id");
        let main_path = dir.join("main.mimi");
        fs::write(&main_path, "// disk placeholder").expect("disk placeholder");
        let file = parse_registered(
            "func main() -> i32 { requires: true\n return 1 }",
            SourceId::new(77),
            SourceRegistry::default(),
        );
        let error = match flow_load_main_with_file(Acc::new(dir.clone()), &main_path, file) {
            Err(error) => error,
            Ok(_) => panic!("known source without a registry record must fail"),
        };
        assert!(error.contains("not present"), "{error}");
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_transitive_imports() {
        let dir = temp_dir("transitive");
        fs::write(
            dir.join("main.mimi"),
            "use a;\nfunc main() -> i32 { a::foo() }",
        )
        .unwrap();
        fs::write(
            dir.join("a.mimi"),
            "use b;\npub func foo() -> i32 { b::bar() }",
        )
        .unwrap();
        fs::write(dir.join("b.mimi"), "pub func bar() -> i32 { 99 }").unwrap();
        assert_load_equivalent(&dir, &dir.join("main.mimi"));
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_nonexistent_file() {
        let dir = temp_dir("nonexist");
        let file_path = dir.join("nope.mimi");
        let acc = Acc::new(dir.clone());
        let result = flow_load_main(acc, &file_path);
        assert!(
            result.is_err(),
            "loading nonexistent file should fail: {:?}",
            result.err()
        );
        cleanup(&dir);
    }

    #[test]
    fn dependency_read_failure_keeps_source_identity_without_faking_a_range() {
        let dir = temp_dir("dependency_read_failure_span");
        let main_path = dir.join("main.mimi");
        let dependency_path = normalize_source_path(&dir.join("missing.mimi"));
        fs::write(&main_path, "use missing;\nfunc main() -> i32 { 0 }").expect("main source");

        let error = flow_load_file_diagnostic(Acc::new(dir.clone()), main_path)
            .expect_err("missing dependency must fail");
        let span = error.diagnostic.span;
        assert!(
            span.source_id.is_known(),
            "dependency identity must be retained"
        );
        assert_eq!(
            (span.start_line, span.start_col, span.end_line, span.end_col),
            (0, 0, 0, 0),
            "an unread source has no honest text range"
        );
        let record = error
            .sources
            .record(span.source_id)
            .expect("dependency source record");
        assert_eq!(record.disk_path.as_deref(), Some(dependency_path.as_path()));
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_circular_dependency() {
        let dir = temp_dir("circular");
        fs::write(dir.join("a.mimi"), "use b;\npub func foo() -> i32 { 1 }").unwrap();
        fs::write(dir.join("b.mimi"), "use a;\npub func bar() -> i32 { 2 }").unwrap();
        let acc = Acc::new(dir.clone());
        let result = flow_load_main(acc, &dir.join("a.mimi"));
        assert!(result.is_err(), "circular dependency should fail");
        let err = format!("{:?}", result.err());
        assert!(
            err.contains("circular"),
            "error should mention circular dependency: {}",
            err
        );
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_deep_diamond() {
        let dir = temp_dir("diamond");
        // A imports B and C; B imports D; C imports D
        fs::write(
            dir.join("a.mimi"),
            "use b;\nuse c;\nfunc main() -> i32 { b::f1() + c::f2() }",
        )
        .unwrap();
        fs::write(
            dir.join("b.mimi"),
            "use d;\npub func f1() -> i32 { d::f3() }",
        )
        .unwrap();
        fs::write(
            dir.join("c.mimi"),
            "use d;\npub func f2() -> i32 { d::f3() }",
        )
        .unwrap();
        fs::write(dir.join("d.mimi"), "pub func f3() -> i32 { 10 }").unwrap();
        assert_load_equivalent(&dir, &dir.join("a.mimi"));
        cleanup(&dir);
    }

    // ── merge_all ───────────────────────────────────────────────────

    #[test]
    fn test_flow_merge_all_empty() {
        let modules = HashMap::new();
        let file = flow_merge_all(&modules).unwrap();
        assert!(file.imports.is_empty());
        assert!(file.items.is_empty());
    }

    #[test]
    fn test_flow_merge_all_duplicate_detection() {
        use crate::ast::{File, FuncDef, Item};
        let mut modules = HashMap::new();
        let item = Item::Func(FuncDef {
            meta: crate::ast::AstNodeMeta::synthetic(crate::ast::AstOrigin::RuntimeSystem(
                "test.loader_fixture",
            )),
            name: "conflict".to_string(),
            pub_: false,
            params: vec![],
            ret: None,
            body: vec![],
            where_clause: vec![],
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
        });
        let file1 = File {
            sources: crate::span::SourceRegistry::default(),
            imports: vec![],
            items: vec![item.clone()],
            implicit_single: false,
        };
        let file2 = File {
            sources: crate::span::SourceRegistry::default(),
            imports: vec![],
            items: vec![item],
            implicit_single: false,
        };
        modules.insert(
            "a".to_string(),
            LoadedModule {
                path: PathBuf::from("a.mimi"),
                file: file1,
            },
        );
        modules.insert(
            "b".to_string(),
            LoadedModule {
                path: PathBuf::from("b.mimi"),
                file: file2,
            },
        );
        let result = flow_merge_all(&modules);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("duplicate"));
    }

    #[test]
    fn flow_merge_all_unions_registries_and_remaps_colliding_dense_ids() {
        let mut sources_a = SourceRegistry::default();
        let source_a = sources_a
            .register_key("workspace:a.mimi", SourceTextOrigin::Disk)
            .expect("a source");
        let file_a = parse_registered(
            "func a() -> i32 { requires: true\n return 1 }",
            source_a,
            sources_a,
        );

        let mut sources_b = SourceRegistry::default();
        let source_b = sources_b
            .register_key("workspace:b.mimi", SourceTextOrigin::Disk)
            .expect("b source");
        let file_b = parse_registered(
            "func b() -> i32 { requires: true\n return 2 }",
            source_b,
            sources_b,
        );
        assert_eq!(source_a, source_b, "both registries allocate dense id 1");

        let modules = HashMap::from([
            (
                "a".to_string(),
                LoadedModule {
                    path: PathBuf::from("a.mimi"),
                    file: file_a,
                },
            ),
            (
                "b".to_string(),
                LoadedModule {
                    path: PathBuf::from("b.mimi"),
                    file: file_b,
                },
            ),
        ]);
        let merged = flow_merge_all(&modules).expect("merge registries");
        let merged_a = requires_source(&merged, "a");
        let merged_b = requires_source(&merged, "b");
        assert_ne!(merged_a, merged_b);
        assert_eq!(
            merged.sources.key(merged_a).map(SourceKey::as_str),
            Some("workspace:a.mimi")
        );
        assert_eq!(
            merged.sources.key(merged_b).map(SourceKey::as_str),
            Some("workspace:b.mimi")
        );
    }

    #[test]
    fn flow_merge_all_remaps_type_metadata_in_declarations_and_expressions() {
        let mut sources_a = SourceRegistry::default();
        let source_a = sources_a
            .register_key("workspace:a-types.mimi", SourceTextOrigin::Disk)
            .expect("a source");
        let file_a = parse_registered("func a() -> i32 { return 1 }", source_a, sources_a);

        let source = r#"func typed(value: List<i32>) -> Result<i64, string> {
    let local: Option<bool> = true
    let casted = 1 as List<i64>
    let info = type_info(Result<bool, string>)
    let closure = fn(arg: List<string>) -> Option<i32> { return arg }
    let turbo = identity::<List<i64>>(value)
    return 1
}"#;
        let mut sources_b = SourceRegistry::default();
        let source_b = sources_b
            .register_key("workspace:b-types.mimi", SourceTextOrigin::Memory)
            .expect("b source");
        let file_b = parse_registered(source, source_b, sources_b);
        assert_eq!(
            source_a, source_b,
            "independent registries both allocate id 1"
        );

        let modules = HashMap::from([
            (
                "a".to_string(),
                LoadedModule {
                    path: PathBuf::from("a-types.mimi"),
                    file: file_a,
                },
            ),
            (
                "b".to_string(),
                LoadedModule {
                    path: PathBuf::from("b-types.mimi"),
                    file: file_b,
                },
            ),
        ]);
        let merged = flow_merge_all(&modules).expect("merge registries");
        let function = merged
            .items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "typed" => Some(function),
                _ => None,
            })
            .expect("typed function");

        let mut type_nodes = Vec::new();
        collect_type_source_ids(&function.params[0].ty, &mut type_nodes);
        collect_type_source_ids(function.ret.as_ref().expect("return type"), &mut type_nodes);

        for stmt in &function.body {
            let Stmt::Let { ty, init, .. } = stmt.unlocated() else {
                continue;
            };
            if let Some(ty) = ty {
                collect_type_source_ids(ty, &mut type_nodes);
            }
            let Some(init) = init else { continue };
            match init.unlocated() {
                Expr::Cast(_, ty) | Expr::TypeInfo(ty) => {
                    collect_type_source_ids(ty, &mut type_nodes);
                }
                Expr::Lambda { params, ret, .. } => {
                    for param in params {
                        collect_type_source_ids(&param.ty, &mut type_nodes);
                    }
                    if let Some(ret) = ret {
                        collect_type_source_ids(ret, &mut type_nodes);
                    }
                }
                Expr::Turbofish(_, type_args, _) => {
                    for ty in type_args {
                        collect_type_source_ids(ty, &mut type_nodes);
                    }
                }
                _ => {}
            }
        }

        assert!(
            type_nodes.len() >= 18,
            "expected nested Type metadata across every syntax entry, got {}",
            type_nodes.len()
        );
        for source_id in type_nodes {
            assert_eq!(
                merged.sources.key(source_id).map(SourceKey::as_str),
                Some("workspace:b-types.mimi"),
                "every Type span must be remapped through the merged registry"
            );
        }
    }
}
