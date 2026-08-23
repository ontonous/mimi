use serde_json::Value;

use crate::ast::{Block, Expr, Item, Pattern, Stmt, Type, TypeDefKind};
use crate::lsp::LspServer;

/// Percent-encode a file path segment for use in a file:// URI.
/// Unlike path.display(), this properly handles spaces, Chinese chars, etc.
fn encode_path_for_uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    // On Unix, path.as_os_str().as_bytes() gives the raw bytes.
    // Percent-encode all bytes that are not unreserved (ALPHA, DIGIT, -, _, ., ~).
    let bytes = path.as_os_str().as_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 3);
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            b'/' => encoded.push('/'),
            _ => {
                encoded.push('%');
                encoded.push_str(&format!("{:02X}", b));
            }
        }
    }
    encoded
}

impl LspServer {
    pub fn compute_document_symbols(&self, text: &str) -> Vec<Value> {
        let mut symbols = Vec::new();

        if let Some(file) = self.parse_with_recovery(text) {
            for item in &file.items {
                match item {
                    Item::Func(f) => {
                        // 0.35.15 (DX backlog #3): AST span replaces the
                        // `func {name}` substring scan.
                        let def_line = f.meta.span.start_line.saturating_sub(1);
                        let keyword_len = "func ".len();
                        symbols.push(serde_json::json!({
                            "name": f.name,
                            "kind": 12, // Function
                            "range": {
                                "start": { "line": def_line, "character": 0 },
                                "end": { "line": def_line, "character": keyword_len + f.name.len() }
                            },
                            "selectionRange": {
                                "start": { "line": def_line, "character": keyword_len },
                                "end": { "line": def_line, "character": keyword_len + f.name.len() }
                            }
                        }));
                    }
                    Item::Type(t) => {
                        // 0.35.15 (DX backlog #3): AST span replaces the
                        // `type {name}` substring scan.
                        let def_line = t.meta.span.start_line.saturating_sub(1);
                        let keyword_len = "type ".len();
                        symbols.push(serde_json::json!({
                            "name": t.name,
                            "kind": 26, // Enum
                            "range": {
                                "start": { "line": def_line, "character": 0 },
                                "end": { "line": def_line, "character": keyword_len + t.name.len() }
                            },
                            "selectionRange": {
                                "start": { "line": def_line, "character": keyword_len },
                                "end": { "line": def_line, "character": keyword_len + t.name.len() }
                            }
                        }));
                    }
                    _ => {}
                }
            }
        }

        symbols
    }

    /// Compute workspace symbols (across all known .mimi files)
    pub fn compute_workspace_symbols(&self, query: &str) -> Vec<Value> {
        let mut symbols = Vec::new();
        let query_lower = query.to_lowercase();

        let mut sources: Vec<(String, String)> = self
            .documents
            .iter()
            .map(|(uri, text)| (uri.clone(), text.clone()))
            .collect();

        if let Some(root) = &self.workspace_root {
            let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());
            if let Ok(entries) = std::fs::read_dir(&root_canon) {
                // Collect into Vec first to drop ReadDir immediately, preventing fd leak.
                let entries: Vec<_> = entries.flatten().collect();
                for entry in entries {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("mimi") {
                        continue;
                    }
                    // P2-7: canonicalize before reading. A symlink inside the
                    // workspace pointing to a .mimi file outside the sandbox
                    // must not be followed for workspace symbols.
                    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                    if !canonical.starts_with(&root_canon) {
                        continue;
                    }
                    let uri = format!("file://{}", encode_path_for_uri(&path));
                    if !self.documents.contains_key(&uri) {
                        if let Ok(text) = crate::path_safety::read_source_capped(&path) {
                            sources.push((uri, text));
                        }
                    }
                }
            }
        }

        for (uri, text) in &sources {
            let file = match self.parse_with_recovery_for_uri(text, Some(uri)) {
                Some(f) => f,
                None => continue,
            };
            for item in &file.items {
                match item {
                    Item::Func(f) => {
                        if !query_lower.is_empty() && !f.name.to_lowercase().contains(&query_lower)
                        {
                            continue;
                        }
                        // 0.35.15 (DX backlog #3): AST span replaces the
                        // `func {name}` substring scan.
                        let def_line = f.meta.span.start_line.saturating_sub(1);
                        symbols.push(ws_symbol(&f.name, 12, uri, def_line, ""));
                    }
                    Item::Type(t) => {
                        if !query_lower.is_empty() && !t.name.to_lowercase().contains(&query_lower)
                        {
                            continue;
                        }
                        // 0.35.15 (DX backlog #3): AST span replaces the
                        // `type {name}` substring scan.
                        let def_line = t.meta.span.start_line.saturating_sub(1);
                        let kind = match &t.kind {
                            TypeDefKind::Record(_) => 23,
                            TypeDefKind::Enum(_) => 10,
                            TypeDefKind::Union(_) => 24,
                            _ => 4,
                        };
                        symbols.push(ws_symbol(&t.name, kind, uri, def_line, ""));
                        if let TypeDefKind::Enum(variants) = &t.kind {
                            for variant in variants {
                                if !query_lower.is_empty()
                                    && !variant.name.to_lowercase().contains(&query_lower)
                                {
                                    continue;
                                }
                                // 0.35.15 (DX backlog #3): variant spans
                                // replace the whole-file name scan (which
                                // landed on the first mention anywhere).
                                let v_line = if variant.meta.span.start_line > 0 {
                                    variant.meta.span.start_line.saturating_sub(1)
                                } else {
                                    def_line
                                };
                                symbols.push(ws_symbol(
                                    &format!("{}::{}", t.name, variant.name),
                                    23,
                                    uri,
                                    v_line,
                                    &t.name,
                                ));
                            }
                        }
                    }
                    Item::Trait(t) => {
                        if !query_lower.is_empty() && !t.name.to_lowercase().contains(&query_lower)
                        {
                            continue;
                        }
                        // 0.35.15 (DX backlog #3): AST span replaces the
                        // `trait {name}` substring scan.
                        let def_line = t.meta.span.start_line.saturating_sub(1);
                        symbols.push(ws_symbol(&t.name, 17, uri, def_line, ""));
                    }
                    Item::Impl(i) => {
                        if !query_lower.is_empty()
                            && !i.type_name.to_lowercase().contains(&query_lower)
                        {
                            continue;
                        }
                        // 0.35.15 (DX backlog #3): AST span replaces the
                        // `contains("impl")` scan.
                        let def_line = i.meta.span.start_line.saturating_sub(1);
                        symbols.push(ws_symbol(&i.type_name, 26, uri, def_line, &i.trait_name));
                    }
                    Item::Actor(a) => {
                        if !query_lower.is_empty() && !a.name.to_lowercase().contains(&query_lower)
                        {
                            continue;
                        }
                        // 0.35.15 (DX backlog #3): AST span replaces the
                        // `actor {name}` substring scan.
                        let def_line = a.meta.span.start_line.saturating_sub(1);
                        symbols.push(ws_symbol(&a.name, 23, uri, def_line, ""));
                    }
                    _ => {}
                }
            }
        }
        symbols
    }

    /// Prepare call hierarchy: find the function at the given position
    pub fn compute_prepare_call_hierarchy(
        &self,
        text: &str,
        uri: &str,
        line: usize,
        character: usize,
    ) -> Vec<Value> {
        let file = match self.parse_with_recovery(text) {
            Some(f) => f,
            None => return vec![],
        };
        let word = self.get_word_at(text, line, character);
        if word.is_empty() {
            return vec![];
        }
        for item in &file.items {
            match item {
                Item::Func(f) if f.name == word => {
                    // 0.35.15 (DX backlog #3): AST span replaces the
                    // `func {name}` substring scan.
                    let def_line = f.meta.span.start_line.saturating_sub(1);
                    let name_char = f.meta.span.start_col.saturating_sub(1) + "func ".len();
                    return vec![serde_json::json!({
                        "name": f.name,
                        "kind": 12,
                        "uri": uri,
                        "range": {
                            "start": { "line": def_line, "character": 0 },
                            "end": { "line": def_line, "character": 0 }
                        },
                        "selectionRange": {
                            "start": { "line": def_line, "character": name_char },
                            "end": { "line": def_line, "character": name_char + f.name.len() }
                        }
                    })];
                }
                Item::Type(t) if t.name == word => {
                    // 0.35.15 (DX backlog #3): AST span replaces the
                    // `type {name}` substring scan.
                    let def_line = t.meta.span.start_line.saturating_sub(1);
                    let name_char = t.meta.span.start_col.saturating_sub(1) + "type ".len();
                    return vec![serde_json::json!({
                        "name": t.name,
                        "kind": match t.kind {
                            TypeDefKind::Record(_) => 23,
                            TypeDefKind::Enum(_) => 10,
                            _ => 4
                        },
                        "uri": uri,
                        "range": {
                            "start": { "line": def_line, "character": 0 },
                            "end": { "line": def_line, "character": 0 }
                        },
                        "selectionRange": {
                            "start": { "line": def_line, "character": name_char },
                            "end": { "line": def_line, "character": name_char + t.name.len() }
                        }
                    })];
                }
                _ => {}
            }
        }
        vec![]
    }
}

/// Build a workspace symbol JSON object
pub(crate) fn ws_symbol(name: &str, kind: u32, uri: &str, line: usize, container: &str) -> Value {
    let mut obj = serde_json::json!({
        "name": name,
        "kind": kind,
        "location": {
            "uri": uri,
            "range": {
                "start": { "line": line, "character": 0 },
                "end": { "line": line, "character": 0 }
            }
        }
    });
    if !container.is_empty() {
        obj["containerName"] = serde_json::Value::String(container.to_string());
    }
    obj
}

/// Count how many times a name appears in code regions of text.
///
/// Kept only for unit-testing the text/`SourceScanner` fallback path. The
/// production code lenses now use `count_ast_references` (A6, 0.36.96).
#[cfg(test)]
pub(crate) fn count_text_references(text: &str, name: &str) -> usize {
    if name.is_empty() {
        return 0;
    }
    let non_code = crate::lsp::util::non_code_byte_ranges(text);
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let line_non_code = non_code.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
            crate::lsp::util::find_word_occurrences(line, name)
                .into_iter()
                .filter(|pos| !crate::lsp::util::byte_in_non_code(line_non_code, *pos))
                .count()
        })
        .sum()
}

// --- A6: AST-based reference counting for code lenses -------------------

/// Count AST identifier occurrences of `name` (excluding each declaration's
/// own name field). Callers add the definition occurrence when they want the
/// historical "definition + references" lens count.
pub(crate) fn count_ast_references(file: &crate::ast::File, name: &str) -> usize {
    let mut count = 0;
    for item in &file.items {
        visit_item_for_refs(item, name, &mut count);
    }
    count
}

fn visit_block_for_refs(block: &Block, name: &str, count: &mut usize) {
    for stmt in block {
        visit_stmt_for_refs(stmt, name, count);
    }
}

fn visit_stmt_for_refs(stmt: &Stmt, name: &str, count: &mut usize) {
    match stmt.unlocated() {
        Stmt::Located { stmt, .. } => visit_stmt_for_refs(stmt, name, count),
        Stmt::Let { pat, ty, init, .. } => {
            visit_pattern_for_refs(pat, name, count);
            if let Some(ty) = ty {
                visit_type_for_refs(ty, name, count);
            }
            if let Some(expr) = init {
                visit_expr_for_refs(expr, name, count);
            }
        }
        Stmt::Return(Some(expr))
        | Stmt::Expr(expr)
        | Stmt::Break(Some(expr))
        | Stmt::Drop(expr) => {
            visit_expr_for_refs(expr, name, count);
        }
        Stmt::Return(None) | Stmt::Break(None) | Stmt::Continue | Stmt::Ellipsis => {}
        Stmt::If { cond, then_, else_ } => {
            visit_expr_for_refs(cond, name, count);
            visit_block_for_refs(then_, name, count);
            if let Some(else_) = else_ {
                visit_block_for_refs(else_, name, count);
            }
        }
        Stmt::IfLet {
            pat,
            init,
            then_,
            else_,
        } => {
            visit_pattern_for_refs(pat, name, count);
            visit_expr_for_refs(init, name, count);
            visit_block_for_refs(then_, name, count);
            if let Some(else_) = else_ {
                visit_block_for_refs(else_, name, count);
            }
        }
        Stmt::While { cond, body } => {
            visit_expr_for_refs(cond, name, count);
            visit_block_for_refs(body, name, count);
        }
        Stmt::WhileLet { pat, init, body } => {
            visit_pattern_for_refs(pat, name, count);
            visit_expr_for_refs(init, name, count);
            visit_block_for_refs(body, name, count);
        }
        Stmt::For {
            var,
            iterable,
            body,
        } => {
            visit_pattern_for_refs(var, name, count);
            visit_expr_for_refs(iterable, name, count);
            visit_block_for_refs(body, name, count);
        }
        Stmt::Loop(body)
        | Stmt::Block(body)
        | Stmt::Arena(body)
        | Stmt::Unsafe(body)
        | Stmt::IeeeFloat(body)
        | Stmt::Defer(body)
        | Stmt::OnFailure(body)
        | Stmt::Parasteps(body) => visit_block_for_refs(body, name, count),
        Stmt::Requires(expr, _) | Stmt::Ensures(expr, _) | Stmt::Invariant(expr, _) => {
            visit_expr_for_refs(expr, name, count);
        }
        Stmt::Math(exprs) => {
            for expr in exprs {
                visit_expr_for_refs(expr, name, count);
            }
        }
        Stmt::Assign { target, value } => {
            visit_expr_for_refs(target, name, count);
            visit_expr_for_refs(value, name, count);
        }
        Stmt::SharedLet { ty, init, .. } => {
            if let Some(ty) = ty {
                visit_type_for_refs(ty, name, count);
            }
            visit_expr_for_refs(init, name, count);
        }
        Stmt::Pinned { expr, body, .. } => {
            visit_expr_for_refs(expr, name, count);
            visit_block_for_refs(body, name, count);
        }
        Stmt::Func(func) => visit_func_for_refs(func, name, count),
    }
}

fn visit_pattern_for_refs(pat: &Pattern, name: &str, count: &mut usize) {
    match &pat.kind {
        crate::ast::PatternKind::Wildcard | crate::ast::PatternKind::Variable(_) => {}
        crate::ast::PatternKind::Literal(_) => {}
        crate::ast::PatternKind::Constructor(ctor, fields) => {
            if ctor == name {
                *count += 1;
            }
            for (_, pat) in fields {
                visit_pattern_for_refs(pat, name, count);
            }
        }
        crate::ast::PatternKind::Tuple(pats) | crate::ast::PatternKind::Array(pats) => {
            for pat in pats {
                visit_pattern_for_refs(pat, name, count);
            }
        }
        crate::ast::PatternKind::Slice(pats, rest) => {
            for pat in pats {
                visit_pattern_for_refs(pat, name, count);
            }
            if let Some(rest) = rest {
                visit_pattern_for_refs(rest, name, count);
            }
        }
    }
}

fn visit_type_for_refs(ty: &Type, name: &str, count: &mut usize) {
    match ty.unlocated() {
        Type::Located { ty, .. } => visit_type_for_refs(ty, name, count),
        Type::Name(n, args) => {
            if n == name {
                *count += 1;
            }
            for arg in args {
                visit_type_for_refs(arg, name, count);
            }
        }
        Type::Ref(_, inner)
        | Type::RefMut(_, inner)
        | Type::Option(inner)
        | Type::CBuffer(inner)
        | Type::Shared(inner)
        | Type::Weak(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner) => {
            visit_type_for_refs(inner, name, count);
        }
        Type::Result(ok, err) => {
            visit_type_for_refs(ok, name, count);
            visit_type_for_refs(err, name, count);
        }
        Type::Tuple(items) => {
            for item in items {
                visit_type_for_refs(item, name, count);
            }
        }
        Type::Func(params, ret) | Type::ExternFunc(params, ret) => {
            for param in params {
                visit_type_for_refs(param, name, count);
            }
            visit_type_for_refs(ret, name, count);
        }
        Type::Cap(n) | Type::CapAtom(n) => {
            if n == name {
                *count += 1;
            }
        }
        Type::Newtype(n, inner) => {
            if n == name {
                *count += 1;
            }
            visit_type_for_refs(inner, name, count);
        }
        Type::Array(inner, _) => visit_type_for_refs(inner, name, count),
        Type::Slice(inner) => visit_type_for_refs(inner, name, count),
        Type::ImplTrait(traits) | Type::DynTrait(traits) => {
            *count += traits.iter().filter(|t| *t == name).count();
        }
        Type::Nothing | Type::Infer | Type::TypeVar(_) | Type::TyErr => {}
        Type::ForAll(_, body) => visit_type_for_refs(body, name, count),
    }
}

fn visit_expr_for_refs(expr: &Expr, name: &str, count: &mut usize) {
    match expr.unlocated() {
        Expr::Located { expr, .. } => visit_expr_for_refs(expr, name, count),
        Expr::Ident(ident) => {
            if ident == name {
                *count += 1;
            }
        }
        Expr::Literal(_) => {}
        Expr::Binary(_, lhs, rhs) | Expr::Index(lhs, rhs) => {
            visit_expr_for_refs(lhs, name, count);
            visit_expr_for_refs(rhs, name, count);
        }
        Expr::Unary(_, e)
        | Expr::Try(e)
        | Expr::Spawn(e)
        | Expr::Await(e)
        | Expr::QuoteInterpolate(e)
        | Expr::TypeOf(e)
        | Expr::Old(e)
        | Expr::OptionalChain(e, _) => {
            visit_expr_for_refs(e, name, count);
        }
        Expr::Call(callee, args) => {
            visit_expr_for_refs(callee, name, count);
            for arg in args {
                visit_expr_for_refs(arg, name, count);
            }
        }
        Expr::Field(e, _) | Expr::TupleIndex(e, _) => visit_expr_for_refs(e, name, count),
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            for item in items {
                visit_expr_for_refs(item, name, count);
            }
        }
        Expr::Comprehension {
            expr,
            var: _,
            iter,
            guard,
        } => {
            visit_expr_for_refs(expr, name, count);
            visit_expr_for_refs(iter, name, count);
            if let Some(guard) = guard {
                visit_expr_for_refs(guard, name, count);
            }
        }
        Expr::Match(scrutinee, arms) => {
            visit_expr_for_refs(scrutinee, name, count);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_expr_for_refs(guard, name, count);
                }
                visit_expr_for_refs(&arm.body, name, count);
            }
        }
        Expr::Record { ty, fields, rest } => {
            if let Some(ty) = ty {
                if ty == name {
                    *count += 1;
                }
            }
            for field in fields {
                visit_expr_for_refs(&field.value, name, count);
            }
            if let Some(rest) = rest {
                visit_expr_for_refs(rest, name, count);
            }
        }
        Expr::Block(block) | Expr::Quote(block) | Expr::Comptime(block) | Expr::Arena(block) => {
            visit_block_for_refs(block, name, count)
        }
        Expr::If { cond, then_, else_ } => {
            visit_expr_for_refs(cond, name, count);
            visit_block_for_refs(then_, name, count);
            if let Some(else_) = else_ {
                visit_block_for_refs(else_, name, count);
            }
        }
        Expr::Lambda { params, ret, body } => {
            for param in params {
                visit_type_for_refs(&param.ty, name, count);
                if let Some(default) = &param.default_value {
                    visit_expr_for_refs(default, name, count);
                }
            }
            if let Some(ret) = ret {
                visit_type_for_refs(ret, name, count);
            }
            visit_block_for_refs(body, name, count);
        }
        Expr::SliceExpr { target, start, end } => {
            visit_expr_for_refs(target, name, count);
            if let Some(start) = start {
                visit_expr_for_refs(start, name, count);
            }
            if let Some(end) = end {
                visit_expr_for_refs(end, name, count);
            }
        }
        Expr::Turbofish(callee, type_args, args) => {
            if callee == name {
                *count += 1;
            }
            for ty in type_args {
                visit_type_for_refs(ty, name, count);
            }
            for arg in args {
                visit_expr_for_refs(arg, name, count);
            }
        }
        Expr::MapLiteral { entries } => {
            for (key, value) in entries {
                visit_expr_for_refs(key, name, count);
                visit_expr_for_refs(value, name, count);
            }
        }
        Expr::NamedArg(_, value) => visit_expr_for_refs(value, name, count),
        Expr::Cast(value, ty) => {
            visit_expr_for_refs(value, name, count);
            visit_type_for_refs(ty, name, count);
        }
        Expr::TypeInfo(ty) => visit_type_for_refs(ty, name, count),
    }
}

fn visit_func_for_refs(func: &crate::ast::FuncDef, name: &str, count: &mut usize) {
    for param in &func.params {
        visit_type_for_refs(&param.ty, name, count);
        if let Some(default) = &param.default_value {
            visit_expr_for_refs(default, name, count);
        }
    }
    if let Some(ret) = &func.ret {
        visit_type_for_refs(ret, name, count);
    }
    visit_block_for_refs(&func.body, name, count);
}

fn visit_item_for_refs(item: &Item, name: &str, count: &mut usize) {
    match item {
        Item::Func(func) => visit_func_for_refs(func, name, count),
        Item::Type(ty) => match &ty.kind {
            TypeDefKind::Alias(t) | TypeDefKind::Newtype(t) => visit_type_for_refs(t, name, count),
            TypeDefKind::Record(fields) | TypeDefKind::Union(fields) => {
                for field in fields {
                    visit_type_for_refs(&field.ty, name, count);
                }
            }
            TypeDefKind::Enum(variants) => {
                for variant in variants {
                    match &variant.payload {
                        None => {}
                        Some(crate::ast::VariantPayload::Tuple(tys)) => {
                            for ty in tys {
                                visit_type_for_refs(ty, name, count);
                            }
                        }
                        Some(crate::ast::VariantPayload::Record(fields)) => {
                            for field in fields {
                                visit_type_for_refs(&field.ty, name, count);
                            }
                        }
                    }
                }
            }
        },
        Item::Actor(actor) => {
            if let Some(flow) = &actor.runs_flow {
                if flow == name {
                    *count += 1;
                }
            }
            for field in &actor.fields {
                visit_type_for_refs(&field.ty, name, count);
                if let Some(init) = &field.init {
                    visit_expr_for_refs(init, name, count);
                }
            }
            for method in &actor.methods {
                visit_func_for_refs(method, name, count);
            }
        }
        Item::Cap(cap) => {
            if let Some(combined) = &cap.combined_with {
                if combined == name {
                    *count += 1;
                }
            }
        }
        Item::Trait(trait_def) => {
            for method in &trait_def.methods {
                for param in &method.params {
                    visit_type_for_refs(&param.ty, name, count);
                    if let Some(default) = &param.default_value {
                        visit_expr_for_refs(default, name, count);
                    }
                }
                if let Some(ret) = &method.ret {
                    visit_type_for_refs(ret, name, count);
                }
            }
        }
        Item::Impl(impl_def) => {
            for ty in &impl_def.trait_args {
                visit_type_for_refs(ty, name, count);
            }
            for ty in &impl_def.type_args {
                visit_type_for_refs(ty, name, count);
            }
            if impl_def.trait_name == name {
                *count += 1;
            }
            if impl_def.type_name == name {
                *count += 1;
            }
            for method in &impl_def.methods {
                visit_func_for_refs(method, name, count);
            }
        }
        Item::ExternBlock(block) => {
            for func in &block.funcs {
                for param in &func.params {
                    visit_type_for_refs(&param.ty, name, count);
                }
                if let Some(ret) = &func.ret {
                    visit_type_for_refs(ret, name, count);
                }
                if let Some(requires) = &func.requires {
                    visit_expr_for_refs(requires, name, count);
                }
                if let Some(ensures) = &func.ensures {
                    visit_expr_for_refs(ensures, name, count);
                }
            }
        }
        Item::Const { ty, value, .. } => {
            if let Some(ty) = ty {
                visit_type_for_refs(ty, name, count);
            }
            visit_expr_for_refs(value, name, count);
        }
        Item::Flow(flow) => {
            if let Some(fault) = &flow.fault_type {
                visit_type_for_refs(fault, name, count);
            }
            for state in &flow.states {
                if let Some(fields) = &state.payload {
                    for field in fields {
                        visit_type_for_refs(&field.ty, name, count);
                    }
                }
            }
            for transition in &flow.transitions {
                for param in &transition.params {
                    visit_type_for_refs(&param.ty, name, count);
                    if let Some(default) = &param.default_value {
                        visit_expr_for_refs(default, name, count);
                    }
                }
                if let Some(fails) = &transition.fails {
                    visit_type_for_refs(fails, name, count);
                }
                if let Some(body) = &transition.body {
                    visit_block_for_refs(body, name, count);
                }
            }
        }
        Item::Session(session) => visit_session_type_for_refs(&session.body, name, count),
    }
}

fn visit_session_type_for_refs(session: &crate::ast::SessionType, name: &str, count: &mut usize) {
    match session.unlocated() {
        crate::ast::SessionType::Located { session, .. } => {
            visit_session_type_for_refs(session, name, count)
        }
        crate::ast::SessionType::Send(ty, cont) | crate::ast::SessionType::Recv(ty, cont) => {
            visit_type_for_refs(ty, name, count);
            visit_session_type_for_refs(cont, name, count);
        }
        crate::ast::SessionType::Dual(cont) => visit_session_type_for_refs(cont, name, count),
        crate::ast::SessionType::Name(n) => {
            if n == name {
                *count += 1;
            }
        }
        crate::ast::SessionType::End => {}
    }
}
