use serde_json::Value;

use crate::ast::{BinOp, Expr, Item, Lit, Pattern, Stmt, Type, TypeDefKind, UnOp};
use crate::lsp::LspServer;

impl LspServer {
    pub fn compute_hover(&self, text: &str, line: usize, character: usize) -> Option<Value> {
        // Get the word at cursor position
        let lines: Vec<&str> = text.lines().collect();
        let current_line = lines.get(line)?;
        let before_cursor: String = current_line.chars().take(character).collect();
        let after_cursor: String = current_line.chars().skip(character).collect();

        // Find word boundaries
        let word_start = before_cursor
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0);
        let word_end = after_cursor
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| character + i)
            .unwrap_or(current_line.len());
        let word = &current_line[word_start..word_end];

        if word.is_empty() {
            return None;
        }

        // Try to parse and find the symbol
        if let Some(file) = self.parse_with_recovery(text) {
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
                        let mut detail = format!("**func** `{}{}({}){}`", word, generics, params.join(", "), ret);
                        // Collect contracts from body
                        let contracts: Vec<String> = f.body.iter().filter_map(|s| {
                            match s {
                                Stmt::Requires(e, _) => Some(format!("  requires: {}", Self::format_contract_expr(e))),
                                Stmt::Ensures(e, _) => Some(format!("  ensures: {}", Self::format_contract_expr(e))),
                                Stmt::Invariant(e, _) => Some(format!("  invariant: {}", Self::format_contract_expr(e))),
                                _ => None,
                            }
                        }).collect();
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
                                        .map(|f| format!("  `{}: {}`", f.name, Self::type_display(&f.ty)))
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
                                detail.push_str(&format!(" (newtype over {})", Self::type_display(inner)));
                            }
                            TypeDefKind::Union(fields) => {
                                if !fields.is_empty() {
                                    let field_strs: Vec<String> = fields
                                        .iter()
                                        .map(|f| format!("  `{}: {}`", f.name, Self::type_display(&f.ty)))
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
                                    .map(|p| format!("{}: {}", p.name, Self::type_display(&p.ty)))
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
                        let method_names: Vec<&str> = a.methods.iter().map(|m| m.name.as_str()).collect();
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
            ("str_char_at", "fn str_char_at(s: string, i: i64) -> string"),
            ("str_substring", "fn str_substring(s: string, start: i64, len: i64) -> string"),
            ("str_parse_int", "fn str_parse_int(s: string) -> (bool, i64)"),
            ("str_parse_float", "fn str_parse_float(s: string) -> (bool, f64)"),
            ("keys", "fn keys(record) -> list"),
            ("values", "fn values(record) -> list"),
            ("has_key", "fn has_key(record, key) -> bool"),
            ("contains", "fn contains(list, elem) -> bool"),
            ("sum", "fn sum(list) -> i64"),
            ("reverse", "fn reverse(list) -> list"),
            ("flatten", "fn flatten(list) -> list"),
            ("str_split", "fn str_split(s: string, sep: string) -> list"),
            ("str_join", "fn str_join(list, sep: string) -> string"),
            ("str_replace", "fn str_replace(s: string, from: string, to: string) -> string"),
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
                format!("{}{}{}", Self::format_contract_expr(lhs), op_str, Self::format_contract_expr(rhs))
            }
            Expr::Unary(UnOp::Not, inner) => format!("!{}", Self::format_contract_expr(inner)),
            Expr::Unary(UnOp::Neg, inner) => format!("-{}", Self::format_contract_expr(inner)),
            Expr::If { cond, then_, else_ } => {
                let then_expr = then_.iter().filter_map(|s| {
                    if let Stmt::Expr(e) = s { Some(Self::format_contract_expr(e)) } else { None }
                }).collect::<Vec<_>>().join("; ");
                let else_expr = else_.as_ref().and_then(|b| b.iter().filter_map(|s| {
                    if let Stmt::Expr(e) = s { Some(Self::format_contract_expr(e)) } else { None }
                }).collect::<Vec<_>>().join("; ").into());
                if let Some(else_s) = else_expr {
                    format!("if {} {{ {} }} else {{ {} }}", Self::format_contract_expr(cond), then_expr, else_s)
                } else {
                    format!("if {} {{ {} }}", Self::format_contract_expr(cond), then_expr)
                }
            }
            Expr::Call(callee, args) => {
                let callee_str = Self::format_contract_expr(callee);
                let args_str: Vec<String> = args.iter().map(Self::format_contract_expr).collect();
                format!("{}({})", callee_str, args_str.join(", "))
            }
            Expr::Field(obj, name) => format!("{}.{}", Self::format_contract_expr(obj), name),
            Expr::Old(inner) => format!("old({})", Self::format_contract_expr(inner)),
            Expr::Tuple(items) => format!("({})", items.iter().map(Self::format_contract_expr).collect::<Vec<_>>().join(", ")),
            Expr::Block(stmts) => {
                let tail: Vec<String> = stmts.iter().filter_map(|s| {
                    match s {
                        Stmt::Expr(e) => Some(Self::format_contract_expr(e)),
                        Stmt::Return(Some(e)) => Some(format!("return {}", Self::format_contract_expr(e))),
                        _ => None,
                    }
                }).collect();
                tail.join("; ")
            }
            Expr::Match(expr, arms) => {
                let arms_str: Vec<String> = arms.iter().map(|arm| {
                    format!("{} => {}", Self::format_pat(&arm.pat), Self::format_contract_expr(&arm.body))
                }).collect();
                format!("match {} {{ {} }}", Self::format_contract_expr(expr), arms_str.join(", "))
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
                return format!("({})", pats.iter().map(Self::format_pat).collect::<Vec<_>>().join(", "));
            }
            Pattern::Array(pats) => {
                return format!("[{}]", pats.iter().map(Self::format_pat).collect::<Vec<_>>().join(", "));
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
        }.to_string()
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
            Type::Result(ok, err) => format!("Result<{}, {}>", Self::type_display(ok), Self::type_display(err)),
            Type::Shared(inner) => format!("shared {}", Self::type_display(inner)),
            Type::LocalShared(inner) => format!("local_shared {}", Self::type_display(inner)),
            Type::Weak(inner) => format!("weak {}", Self::type_display(inner)),
            Type::WeakLocal(inner) => format!("weak_local {}", Self::type_display(inner)),
            Type::Newtype(name, inner) => format!("{} (newtype over {})", name, Self::type_display(inner)),
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
        }
    }
}
