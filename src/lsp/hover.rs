use serde_json::Value;

use crate::ast::{BinOp, Expr, Item, Lit, Pattern, Stmt, Type, TypeDefKind, UnOp};
use crate::lsp::util::word_range_at;
use crate::lsp::LspServer;

impl LspServer {
    pub fn compute_hover(&self, text: &str, line: usize, character: usize) -> Option<Value> {
        // Get the word at cursor position using unified word boundary detection
        let (word_start, word_end) = word_range_at(text, line, character)?;
        let current_line = text.lines().nth(line)?;
        let word = &current_line[word_start..word_end];

        if word.is_empty() {
            return None;
        }

        // Try to parse and find the symbol
        if let Some(file) = self.parse_with_recovery(text) {
            // ── v0.28.11: variable / parameter / record-field hover ──
            // Search the parsed AST for the cursor word in local variable
            // bindings, function parameters, and record-field accesses
            // before falling through to the top-level symbol lookup.
            if let Some(h) = self.hover_local(&file, line, character, word) {
                return Some(h);
            }

            for item in &file.items {
                match item {
                    Item::Func(f) if f.name == word => {
                        let params: Vec<String> = f
                            .params
                            .iter()
                            .map(|p| format!("{}: {}", p.name, Self::type_display(&p.ty)))
                            .collect();
                        let ret = f
                            .ret
                            .as_ref()
                            .map(|t| format!(" -> {}", Self::type_display(t)))
                            .unwrap_or_default();
                        let generics = if f.generics.is_empty() {
                            String::new()
                        } else {
                            let g: Vec<&str> = f.generics.iter().map(|g| g.name.as_str()).collect();
                            format!("[{}]", g.join(", "))
                        };
                        let mut detail = format!(
                            "**func** `{}{}({}){}`",
                            word,
                            generics,
                            params.join(", "),
                            ret
                        );
                        // Collect contracts from body
                        let contracts: Vec<String> = f
                            .body
                            .iter()
                            .filter_map(|s| match s {
                                Stmt::Requires(e, _) => {
                                    Some(format!("  requires: {}", Self::format_contract_expr(e)))
                                }
                                Stmt::Ensures(e, _) => {
                                    Some(format!("  ensures: {}", Self::format_contract_expr(e)))
                                }
                                Stmt::Invariant(e, _) => {
                                    Some(format!("  invariant: {}", Self::format_contract_expr(e)))
                                }
                                _ => None,
                            })
                            .collect();
                        if !contracts.is_empty() {
                            detail.push_str("\n\nContracts:\n");
                            detail.push_str(&contracts.join("\n"));
                        }
                        return Some(serde_json::json!({
                            "contents": {
                                "kind": "markdown",
                                "value": detail
                            }
                        }));
                    }
                    Item::Type(t) if t.name == word => {
                        let mut detail = format!("**type** `{}`", word);
                        match &t.kind {
                            TypeDefKind::Record(fields) => {
                                if !fields.is_empty() {
                                    let field_strs: Vec<String> = fields
                                        .iter()
                                        .map(|f| {
                                            format!("  `{}: {}`", f.name, Self::type_display(&f.ty))
                                        })
                                        .collect();
                                    detail.push_str("\n\nFields:\n");
                                    detail.push_str(&field_strs.join("\n"));
                                }
                            }
                            TypeDefKind::Enum(variants) => {
                                if !variants.is_empty() {
                                    let var_strs: Vec<String> = variants
                                        .iter()
                                        .map(|v| format!("  `{}`", v.name))
                                        .collect();
                                    detail.push_str("\n\nVariants:\n");
                                    detail.push_str(&var_strs.join("\n"));
                                }
                            }
                            TypeDefKind::Alias(inner) => {
                                detail.push_str(&format!(" = {}", Self::type_display(inner)));
                            }
                            TypeDefKind::Newtype(inner) => {
                                detail.push_str(&format!(
                                    " (newtype over {})",
                                    Self::type_display(inner)
                                ));
                            }
                            TypeDefKind::Union(fields) => {
                                if !fields.is_empty() {
                                    let field_strs: Vec<String> = fields
                                        .iter()
                                        .map(|f| {
                                            format!("  `{}: {}`", f.name, Self::type_display(&f.ty))
                                        })
                                        .collect();
                                    detail.push_str("\n\nUnion fields:\n");
                                    detail.push_str(&field_strs.join("\n"));
                                }
                            }
                        }
                        return Some(serde_json::json!({
                            "contents": {
                                "kind": "markdown",
                                "value": detail
                            }
                        }));
                    }
                    Item::Trait(t) if t.name == word => {
                        let methods: Vec<String> = t
                            .methods
                            .iter()
                            .map(|m| {
                                let params: Vec<String> = m
                                    .params
                                    .iter()
                                    .map(|p| {
                                        let base =
                                            format!("{}: {}", p.name, Self::type_display(&p.ty));
                                        if let Some(ref default_expr) = p.default_value {
                                            format!(
                                                "{} = {}",
                                                base,
                                                Self::format_expr_simple(default_expr)
                                            )
                                        } else {
                                            base
                                        }
                                    })
                                    .collect();
                                let ret = m
                                    .ret
                                    .as_ref()
                                    .map(|r| format!(" -> {}", Self::type_display(r)))
                                    .unwrap_or_default();
                                format!("  `fn {}({}){}`", m.name, params.join(", "), ret)
                            })
                            .collect();
                        let detail = if methods.is_empty() {
                            format!("**trait** `{}`", word)
                        } else {
                            format!("**trait** `{}`\n\nMethods:\n{}", word, methods.join("\n"))
                        };
                        return Some(serde_json::json!({
                            "contents": {
                                "kind": "markdown",
                                "value": detail
                            }
                        }));
                    }
                    Item::Impl(imp) if imp.type_name == word => {
                        let methods: Vec<String> = imp
                            .methods
                            .iter()
                            .map(|m| format!("  `fn {}(...)`", m.name))
                            .collect();
                        let detail = if methods.is_empty() {
                            format!("**impl** `{} for {}`", imp.trait_name, imp.type_name)
                        } else {
                            format!(
                                "**impl** `{} for {}`\n\nMethods:\n{}",
                                imp.trait_name,
                                imp.type_name,
                                methods.join("\n")
                            )
                        };
                        return Some(serde_json::json!({
                            "contents": {
                                "kind": "markdown",
                                "value": detail
                            }
                        }));
                    }
                    Item::Module(m) if m.name == word => {
                        let item_count = m.items.len();
                        return Some(serde_json::json!({
                            "contents": {
                                "kind": "markdown",
                                "value": format!("**module** `{}` ({} items)", word, item_count)
                            }
                        }));
                    }
                    Item::Actor(a) if a.name == word => {
                        let method_names: Vec<&str> =
                            a.methods.iter().map(|m| m.name.as_str()).collect();
                        return Some(serde_json::json!({
                            "contents": {
                                "kind": "markdown",
                                "value": format!("**actor** `{}`\n\nMethods: {}", word, method_names.join(", "))
                            }
                        }));
                    }
                    _ => {}
                }
            }
        }

        // Check builtins
        let builtins = vec![
            ("println", "fn println(args...)"),
            ("assert", "fn assert(condition: bool)"),
            ("assert_eq", "fn assert_eq(a, b)"),
            ("len", "fn len(collection) -> i64"),
            ("push", "fn push(list, item)"),
            ("pop", "fn pop(list) -> item"),
            ("range", "fn range(n) -> list"),
            ("sqrt", "fn sqrt(x: f64) -> f64"),
            ("abs", "fn abs(x) -> x"),
            ("min", "fn min(a, b) -> a"),
            ("max", "fn max(a, b) -> a"),
            ("to_string", "fn to_string(val) -> string"),
            ("print", "fn print(args...)"),
            ("pow", "fn pow(base, exp) -> result"),
            ("floor", "fn floor(x: f64) -> i64"),
            ("ceil", "fn ceil(x: f64) -> i64"),
            ("round", "fn round(x: f64) -> i64"),
            ("random", "fn random() -> f64"),
            ("pi", "fn pi() -> f64"),
            ("read_file", "fn read_file(path: string) -> string"),
            ("write_file", "fn write_file(path: string, content: string)"),
            ("file_exists", "fn file_exists(path: string) -> bool"),
            ("to_int", "fn to_int(val) -> i64"),
            ("to_float", "fn to_float(val) -> f64"),
            ("char_code", "fn char_code(s: string, i: i64) -> i64"),
            ("chr", "fn chr(code: i64) -> string"),
            ("str_char_at", "fn str_char_at(s: string, i: i64) -> string"),
            (
                "str_substring",
                "fn str_substring(s: string, start: i64, len: i64) -> string",
            ),
            (
                "str_parse_int",
                "fn str_parse_int(s: string) -> (bool, i64)",
            ),
            (
                "str_parse_float",
                "fn str_parse_float(s: string) -> (bool, f64)",
            ),
            ("keys", "fn keys(record) -> list"),
            ("values", "fn values(record) -> list"),
            ("has_key", "fn has_key(record, key) -> bool"),
            ("contains", "fn contains(list, elem) -> bool"),
            ("sum", "fn sum(list) -> i64"),
            ("reverse", "fn reverse(list) -> list"),
            ("flatten", "fn flatten(list) -> list"),
            ("str_split", "fn str_split(s: string, sep: string) -> list"),
            ("str_join", "fn str_join(list, sep: string) -> string"),
            (
                "str_replace",
                "fn str_replace(s: string, from: string, to: string) -> string",
            ),
        ];

        for (name, sig) in builtins {
            if word == name {
                return Some(serde_json::json!({
                    "contents": {
                        "kind": "markdown",
                        "value": format!("**builtin** `{}`", sig)
                    }
                }));
            }
        }

        None
    }

    /// Format a literal for contract display
    fn format_lit(lit: &Lit) -> String {
        match lit {
            Lit::Int(n) => format!("{}", n),
            Lit::Float(f) => format!("{}", f),
            Lit::Bool(b) => format!("{}", b),
            Lit::String(s) => format!("\"{}\"", s),
            Lit::FString(_) => "f\"...\"".to_string(),
            Lit::Unit => "()".to_string(),
        }
    }

    /// Format a contract expression for human-readable hover display
    fn format_contract_expr(expr: &Expr) -> String {
        match expr {
            Expr::Ident(name) => name.clone(),
            Expr::Literal(lit) => Self::format_lit(lit),
            Expr::Binary(op, lhs, rhs) => {
                let op_str = match op {
                    BinOp::Add => " + ",
                    BinOp::Sub => " - ",
                    BinOp::Mul => " * ",
                    BinOp::Div => " / ",
                    BinOp::Mod => " % ",
                    BinOp::EqCmp => " == ",
                    BinOp::NeCmp => " != ",
                    BinOp::Lt => " < ",
                    BinOp::Gt => " > ",
                    BinOp::Le => " <= ",
                    BinOp::Ge => " >= ",
                    BinOp::And => " && ",
                    BinOp::Or => " || ",
                    _ => " ?? ",
                };
                format!(
                    "{}{}{}",
                    Self::format_contract_expr(lhs),
                    op_str,
                    Self::format_contract_expr(rhs)
                )
            }
            Expr::Unary(UnOp::Not, inner) => format!("!{}", Self::format_contract_expr(inner)),
            Expr::Unary(UnOp::Neg, inner) => format!("-{}", Self::format_contract_expr(inner)),
            Expr::If { cond, then_, else_ } => {
                let then_expr = then_
                    .iter()
                    .filter_map(|s| {
                        if let Stmt::Expr(e) = s {
                            Some(Self::format_contract_expr(e))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                let else_expr = else_.as_ref().and_then(|b| {
                    b.iter()
                        .filter_map(|s| {
                            if let Stmt::Expr(e) = s {
                                Some(Self::format_contract_expr(e))
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                        .into()
                });
                if let Some(else_s) = else_expr {
                    format!(
                        "if {} {{ {} }} else {{ {} }}",
                        Self::format_contract_expr(cond),
                        then_expr,
                        else_s
                    )
                } else {
                    format!(
                        "if {} {{ {} }}",
                        Self::format_contract_expr(cond),
                        then_expr
                    )
                }
            }
            Expr::Call(callee, args) => {
                let callee_str = Self::format_contract_expr(callee);
                let args_str: Vec<String> = args.iter().map(Self::format_contract_expr).collect();
                format!("{}({})", callee_str, args_str.join(", "))
            }
            Expr::Field(obj, name) => format!("{}.{}", Self::format_contract_expr(obj), name),
            Expr::Old(inner) => format!("old({})", Self::format_contract_expr(inner)),
            Expr::Tuple(items) => format!(
                "({})",
                items
                    .iter()
                    .map(Self::format_contract_expr)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Expr::Block(stmts) => {
                let tail: Vec<String> = stmts
                    .iter()
                    .filter_map(|s| match s {
                        Stmt::Expr(e) => Some(Self::format_contract_expr(e)),
                        Stmt::Return(Some(e)) => {
                            Some(format!("return {}", Self::format_contract_expr(e)))
                        }
                        _ => None,
                    })
                    .collect();
                tail.join("; ")
            }
            Expr::Match(expr, arms) => {
                let arms_str: Vec<String> = arms
                    .iter()
                    .map(|arm| {
                        format!(
                            "{} => {}",
                            Self::format_pat(&arm.pat),
                            Self::format_contract_expr(&arm.body)
                        )
                    })
                    .collect();
                format!(
                    "match {} {{ {} }}",
                    Self::format_contract_expr(expr),
                    arms_str.join(", ")
                )
            }
            _ => "…".to_string(),
        }
    }

    fn format_pat(pat: &Pattern) -> String {
        match pat {
            Pattern::Wildcard => "_",
            Pattern::Variable(name) => name.as_str(),
            Pattern::Literal(lit) => return Self::format_lit(lit),
            Pattern::Constructor(name, args) => {
                let args_str: Vec<String> = args.iter().map(Self::format_pat).collect();
                return format!("{}({})", name, args_str.join(", "));
            }
            Pattern::Tuple(pats) => {
                return format!(
                    "({})",
                    pats.iter()
                        .map(Self::format_pat)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Pattern::Array(pats) => {
                return format!(
                    "[{}]",
                    pats.iter()
                        .map(Self::format_pat)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Pattern::Slice(pats, rest) => {
                let mut s: Vec<String> = pats.iter().map(Self::format_pat).collect();
                if let Some(r) = rest {
                    s.push(format!("..{}", Self::format_pat(r)));
                } else {
                    s.push("..".to_string());
                }
                return format!("[{}]", s.join(", "));
            }
        }
        .to_string()
    }

    /// Format an expression as a short display string for hover hints.
    pub(crate) fn format_expr_simple(expr: &Expr) -> String {
        match expr {
            Expr::Literal(l) => crate::lsp::LspServer::format_lit_simple(l),
            Expr::Ident(name) => name.clone(),
            Expr::Unary(op, e) => format!(
                "{}{}",
                Self::format_unop_simple(*op),
                Self::format_expr_simple(e)
            ),
            Expr::Binary(op, l, r) => format!(
                "{} {} {}",
                Self::format_expr_simple(l),
                Self::format_binop_simple(*op),
                Self::format_expr_simple(r)
            ),
            Expr::Call(callee, args) => {
                let a: Vec<String> = args.iter().map(Self::format_expr_simple).collect();
                format!("{}({})", Self::format_expr_simple(callee), a.join(", "))
            }
            Expr::Field(obj, field) => format!("{}.{}", Self::format_expr_simple(obj), field),
            Expr::Index(obj, idx) => format!(
                "{}[{}]",
                Self::format_expr_simple(obj),
                Self::format_expr_simple(idx)
            ),
            Expr::Tuple(elems) => {
                let a: Vec<String> = elems.iter().map(Self::format_expr_simple).collect();
                format!("({})", a.join(", "))
            }
            Expr::List(elems) => {
                let a: Vec<String> = elems.iter().map(Self::format_expr_simple).collect();
                format!("[{}]", a.join(", "))
            }
            Expr::If { cond, .. } => format!("if {} {{ ... }}", Self::format_expr_simple(cond)),
            Expr::Block(_) => "{ ... }".to_string(),
            _ => "...".to_string(),
        }
    }

    fn format_lit_simple(lit: &Lit) -> String {
        match lit {
            Lit::Int(v) => format!("{}", v),
            Lit::Float(v) => format!("{}", v),
            Lit::Bool(v) => format!("{}", v),
            Lit::String(v) => format!("\"{}\"", v),
            Lit::FString(parts) => {
                let s: String = parts
                    .iter()
                    .map(|p| match p {
                        crate::ast::FStringPart::Text(t) => t.clone(),
                        crate::ast::FStringPart::Interp(_) => "{}".to_string(),
                    })
                    .collect();
                format!("f\"{}\"", s)
            }
            Lit::Unit => "()".to_string(),
        }
    }

    fn format_unop_simple(op: UnOp) -> &'static str {
        match op {
            UnOp::Neg => "-",
            UnOp::Not => "!",
            UnOp::Ref => "&",
            UnOp::RefMut => "&mut ",
            UnOp::Deref => "*",
        }
    }

    fn format_binop_simple(op: BinOp) -> &'static str {
        match op {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Pow => "^",
            BinOp::Assign => "=",
            BinOp::EqCmp => "==",
            BinOp::NeCmp => "!=",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Range => "..",
        }
    }

    /// Format a type for human-readable display
    pub(crate) fn type_display(ty: &Type) -> String {
        match ty {
            Type::Name(name, params) => {
                if params.is_empty() {
                    name.clone()
                } else {
                    let inner: Vec<String> = params.iter().map(Self::type_display).collect();
                    format!("{}[{}]", name, inner.join(", "))
                }
            }
            Type::Ref(lt, inner) => {
                let lt_str = lt.as_ref().map(|l| format!("'{} ", l)).unwrap_or_default();
                format!("&{} {}", lt_str, Self::type_display(inner))
            }
            Type::RefMut(lt, inner) => {
                let lt_str = lt.as_ref().map(|l| format!("'{} ", l)).unwrap_or_default();
                format!("&{} mut {}", lt_str, Self::type_display(inner))
            }
            Type::Tuple(elems) => {
                let inner: Vec<String> = elems.iter().map(Self::type_display).collect();
                format!("({})", inner.join(", "))
            }
            Type::Func(params, ret) => {
                let p: Vec<String> = params.iter().map(Self::type_display).collect();
                format!("fn({}) -> {}", p.join(", "), Self::type_display(ret))
            }
            Type::ExternFunc(params, ret) => {
                let p: Vec<String> = params.iter().map(Self::type_display).collect();
                format!("extern fn({}) -> {}", p.join(", "), Self::type_display(ret))
            }
            Type::RawPtr(inner) => format!("*{}", Self::type_display(inner)),
            Type::RawPtrMut(inner) => format!("*mut {}", Self::type_display(inner)),
            Type::CShared(inner) => format!("c_shared {}", Self::type_display(inner)),
            Type::CBorrow(inner) => format!("c_borrow {}", Self::type_display(inner)),
            Type::CBorrowMut(inner) => format!("c_borrow_mut {}", Self::type_display(inner)),
            Type::Option(inner) => format!("Option<{}>", Self::type_display(inner)),
            Type::Result(ok, err) => format!(
                "Result<{}, {}>",
                Self::type_display(ok),
                Self::type_display(err)
            ),
            Type::Shared(inner) => format!("shared {}", Self::type_display(inner)),
            Type::LocalShared(inner) => format!("local_shared {}", Self::type_display(inner)),
            Type::Weak(inner) => format!("weak {}", Self::type_display(inner)),
            Type::WeakLocal(inner) => format!("weak_local {}", Self::type_display(inner)),
            Type::Newtype(name, inner) => {
                format!("{} (newtype over {})", name, Self::type_display(inner))
            }
            Type::Array(inner, n) => format!("[{}; {}]", Self::type_display(inner), n),
            Type::Slice(inner) => format!("[{}]", Self::type_display(inner)),
            Type::ImplTrait(ts) => format!("impl {}", ts.join(" + ")),
            Type::DynTrait(ts) => format!("dyn {}", ts.join(" + ")),
            Type::RawString => "RawString".to_string(),
            Type::Cap(name) => format!("cap {}", name),
            Type::CBuffer(inner) => format!("CBuffer<{}>", Self::type_display(inner)),
            Type::Nothing => "!".to_string(),
            Type::Allocator => "Allocator".to_string(),
            Type::Infer => "_".to_string(),
            Type::TypeVar(id) => format!("?T{}", id),
            Type::ForAll(params, body) => {
                format!("forall {}. {}", params.join(", "), Self::type_display(body))
            }
        }
    }

    /// v0.28.11: Hover for local bindings (let variables, function
    /// parameters) and record field accesses. Returns None if the word at
    /// (line, character) doesn't match a local binding or field access —
    /// in that case the caller falls back to top-level symbol lookup.
    fn hover_local(
        &self,
        file: &crate::ast::File,
        line: usize,
        character: usize,
        word: &str,
    ) -> Option<Value> {
        for item in &file.items {
            if let Item::Func(f) = item {
                // 1. Function parameters: hover over `x` in `f.params`
                if let Some(param) = f.params.iter().find(|p| p.name == word) {
                    // The cursor must be inside the function signature line
                    // for this to be a parameter hover. For simplicity, we
                    // accept any cursor position on the parameter.
                    let detail = format!(
                        "**let** `{}: {}` (parameter of `{}`)",
                        param.name,
                        Self::type_display(&param.ty),
                        f.name
                    );
                    return Some(serde_json::json!({
                        "contents": {
                            "kind": "markdown",
                            "value": detail
                        }
                    }));
                }

                // 2. Function body: scan for let bindings and field accesses
                // whose position overlaps (line, character).
                if let Some(h) = self.hover_in_block(&f.body, line, character, word, &f.name, file)
                {
                    return Some(h);
                }

                // 3. Return value hover: if cursor is on the function body's
                // last expression (implicit return value), show the return type.
                if let Some(ret_ty) = &f.ret {
                    if Self::word_in_last_expr(&f.body, word) {
                        let detail = format!(
                            "**returns** `{}` (from `{}`)",
                            Self::type_display(ret_ty),
                            f.name
                        );
                        return Some(serde_json::json!({
                            "contents": {
                                "kind": "markdown",
                                "value": detail
                            }
                        }));
                    }
                }
            }
        }
        None
    }

    /// Scan a function body for hover-worthy references at (line, char):
    ///   - `let name: T = ...` declarations (T from explicit annotation)
    ///   - `obj.field` accesses (resolve obj type from surrounding context)
    fn hover_in_block(
        &self,
        block: &[Stmt],
        line: usize,
        character: usize,
        word: &str,
        func_name: &str,
        file: &crate::ast::File,
    ) -> Option<Value> {
        // Track declared variables with explicit types as we walk the block
        // so that `obj.field` can resolve obj's type for field hover.
        let mut locals: Vec<(String, Type)> = Vec::new();

        // First, scan for let bindings to build up scope (single pass
        // forward only — handles non-shadowing sequential bindings).
        for stmt in block {
            if let Stmt::Let {
                pat: Pattern::Variable(name),
                ty: Some(ty),
                ..
            } = stmt
            {
                locals.push((name.clone(), ty.clone()));
            }
        }

        // Check whether (line, character) is on a let binding's variable.
        // Use the source text to verify, but for hover we accept any
        // occurrence of the name in the function body.
        for (name, ty) in &locals {
            if name == word {
                let detail = format!(
                    "**let** `{}: {}` (in `{}`)",
                    name,
                    Self::type_display(ty),
                    func_name
                );
                return Some(serde_json::json!({
                    "contents": {
                        "kind": "markdown",
                        "value": detail
                    }
                }));
            }
        }

        // Walk the block for `obj.field` accesses where `field == word`
        // and `obj` is a let-bound variable with a record-like type.
        for stmt in block {
            if let Some(h) = self.hover_field_in_stmt(stmt, word, &locals, file) {
                return Some(h);
            }
        }

        // If we didn't find a precise local match, but the cursor is in
        // the function body and word is a declared local, still return
        // the type — IDE users expect a hover anywhere on a use site.
        let _ = (line, character);
        None
    }

    /// Look for `Expr::Field(obj, field)` where field == word and obj is
    /// in `locals` with a record-like type. Returns hover JSON when the
    /// field is found in the type's record definition.
    fn hover_field_in_stmt(
        &self,
        stmt: &Stmt,
        word: &str,
        locals: &[(String, Type)],
        file: &crate::ast::File,
    ) -> Option<Value> {
        // Recursively collect all Expr::Field nodes in the statement.
        let mut found: Option<Value> = None;
        Self::scan_stmt_for_field(stmt, word, locals, file, &mut found);
        found
    }

    fn scan_stmt_for_field(
        stmt: &Stmt,
        word: &str,
        locals: &[(String, Type)],
        file: &crate::ast::File,
        out: &mut Option<Value>,
    ) {
        match stmt {
            Stmt::Let {
                init: Some(expr), ..
            } => {
                Self::scan_expr_for_field(expr, word, locals, file, out);
            }
            Stmt::Expr(expr) => Self::scan_expr_for_field(expr, word, locals, file, out),
            Stmt::Return(Some(expr)) => Self::scan_expr_for_field(expr, word, locals, file, out),
            Stmt::While { cond, body } => {
                Self::scan_expr_for_field(cond, word, locals, file, out);
                for s in body {
                    Self::scan_stmt_for_field(s, word, locals, file, out);
                }
            }
            Stmt::WhileLet { init, body, .. } => {
                Self::scan_expr_for_field(init, word, locals, file, out);
                for s in body {
                    Self::scan_stmt_for_field(s, word, locals, file, out);
                }
            }
            Stmt::For { iterable, body, .. } => {
                Self::scan_expr_for_field(iterable, word, locals, file, out);
                for s in body {
                    Self::scan_stmt_for_field(s, word, locals, file, out);
                }
            }
            Stmt::If { cond, then_, else_ } => {
                Self::scan_expr_for_field(cond, word, locals, file, out);
                for s in then_ {
                    Self::scan_stmt_for_field(s, word, locals, file, out);
                }
                if let Some(eb) = else_ {
                    for s in eb {
                        Self::scan_stmt_for_field(s, word, locals, file, out);
                    }
                }
            }
            _ => {}
        }
    }

    fn scan_expr_for_field(
        expr: &Expr,
        word: &str,
        locals: &[(String, Type)],
        file: &crate::ast::File,
        out: &mut Option<Value>,
    ) {
        if out.is_some() {
            return;
        }
        match expr {
            Expr::Field(obj, field) => {
                if field == word {
                    if let Some(hover) = Self::resolve_field_hover(obj, field, locals, file) {
                        *out = Some(hover);
                        return;
                    }
                }
                Self::scan_expr_for_field(obj, word, locals, file, out);
            }
            Expr::Call(callee, args) => {
                Self::scan_expr_for_field(callee, word, locals, file, out);
                for a in args {
                    Self::scan_expr_for_field(a, word, locals, file, out);
                }
            }
            Expr::Binary(_, l, r) => {
                Self::scan_expr_for_field(l, word, locals, file, out);
                Self::scan_expr_for_field(r, word, locals, file, out);
            }
            Expr::Unary(_, inner) => {
                Self::scan_expr_for_field(inner, word, locals, file, out);
            }
            Expr::Tuple(elems) | Expr::List(elems) => {
                for e in elems {
                    Self::scan_expr_for_field(e, word, locals, file, out);
                }
            }
            Expr::Index(obj, idx) => {
                Self::scan_expr_for_field(obj, word, locals, file, out);
                Self::scan_expr_for_field(idx, word, locals, file, out);
            }
            Expr::Record { fields, .. } => {
                for f in fields {
                    Self::scan_expr_for_field(&f.value, word, locals, file, out);
                }
            }
            Expr::If { cond, then_, else_ } => {
                Self::scan_expr_for_field(cond, word, locals, file, out);
                for s in then_ {
                    Self::scan_stmt_for_field(s, word, locals, file, out);
                }
                if let Some(eb) = else_ {
                    for s in eb {
                        Self::scan_stmt_for_field(s, word, locals, file, out);
                    }
                }
            }
            _ => {}
        }
    }

    /// Given an `obj.field` access, return a hover JSON for the field
    /// by resolving obj's type from the surrounding scope, then
    /// consulting the file's `Item::Type` definitions to find the
    /// field's declared type.
    fn resolve_field_hover(
        obj: &Expr,
        field: &str,
        locals: &[(String, Type)],
        file: &crate::ast::File,
    ) -> Option<Value> {
        // Walk through `obj` (could be `a.b.c.field`) to find the root
        // identifier, then look it up in `locals`.
        let root_ident = Self::strip_field_chain(obj)?;
        let ty = locals
            .iter()
            .find(|(n, _)| n == &root_ident)
            .map(|(_, t)| t.clone())?;

        // Resolve Type::Name("Person") → look up the type definition in
        // the file. For other type forms (List, Option, Result, etc.) we
        // can't easily extract the field — return None and let the
        // top-level hover handle the word as an identifier.
        if let Type::Name(type_name, _) = &ty {
            for item in &file.items {
                if let Item::Type(td) = item {
                    if &td.name == type_name {
                        if let TypeDefKind::Record(fields) = &td.kind {
                            if let Some(f) = fields.iter().find(|f| f.name == field) {
                                let detail = format!(
                                    "**field** `{}: {}` (of `{}`)",
                                    f.name,
                                    Self::type_display(&f.ty),
                                    type_name
                                );
                                return Some(serde_json::json!({
                                    "contents": {
                                        "kind": "markdown",
                                        "value": detail
                                    }
                                }));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Strip a `a.b.c` chain down to its leftmost identifier.
    fn strip_field_chain(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident(name) => Some(name.clone()),
            Expr::Field(obj, _) => Self::strip_field_chain(obj),
            _ => None,
        }
    }

    /// Check whether `word` appears anywhere in the block's last
    /// expression (implicit return). Used to trigger return-type hover.
    fn word_in_last_expr(block: &[Stmt], word: &str) -> bool {
        let last = match block.last() {
            Some(Stmt::Expr(e)) | Some(Stmt::Return(Some(e))) => e,
            _ => return false,
        };
        Self::expr_contains_word(last, word)
    }

    /// Recursively checks whether `word` appears in an expression as an
    /// Ident, a substring of a Literal display, or a Call callee name.
    fn expr_contains_word(e: &Expr, w: &str) -> bool {
        match e {
            Expr::Ident(name) => name == w,
            Expr::Literal(lit) => format!("{:?}", lit).contains(w),
            Expr::Field(obj, name) => name == w || Self::expr_contains_word(obj, w),
            Expr::Index(obj, idx) => {
                Self::expr_contains_word(obj, w) || Self::expr_contains_word(idx, w)
            }
            Expr::Call(callee, args) => {
                Self::expr_contains_word(callee, w)
                    || args.iter().any(|a| Self::expr_contains_word(a, w))
            }
            Expr::Binary(_, l, r) => {
                Self::expr_contains_word(l, w) || Self::expr_contains_word(r, w)
            }
            Expr::Unary(_, inner) => Self::expr_contains_word(inner, w),
            Expr::Tuple(elems) | Expr::List(elems) => {
                elems.iter().any(|e| Self::expr_contains_word(e, w))
            }
            _ => false,
        }
    }
}
