//! 0.36.39 (Phase C — 泛型×线性单态化，切片 1)：线性黑盒直通判定。
//!
//! 背景（AGENTS.md §0 H2 / §2.3 裁决）：泛型参数不被线性追踪
//! （GenericParameter `is_linear() == false`）——线性值（cap / SessionChan /
//! Flow state / 含线性元素的容器）流入泛型调用会逃逸 exactly-once 强制，故
//! 全量 E0432 拒绝。0.36.36-38 已把"名字级"追踪做实（call-site 具体类型驱动：
//! `capability_places` 按实参具体类型移动、返回绑定按 call-site 实例化类型
//! 追踪——E0432 放行后 probe 实证 `let d = pass_through(c); drop(d)` 全链正确，
//! 漏 drop 会 E0256）。缺口只剩"调体是否可能静默弃值"。
//!
//! 本模块判定：泛型函数第 `param_index` 个参数是否**线性黑盒健全**——调体对
//! T 的线性性零依赖：每条路径要么把值转移出去（return，可嵌在容器/元组内，或
//! 移入另一个"线性安全接收者"），要么显式 drop，绝不静默丢弃、绝不做依赖消费
//! 形状的析构。此判定对任意具体线性实例化都健全（决策与 T 无关），从而放行
//! "直通 / 整容器移交 / drop"类调用，同时 fail-closed 拒绝"丢弃 / 半丢弃 /
//! 元素析构 / 条件路径依赖"类（后者留给 per-instantiation 切片 2，仍 E0432）。
//!
//! 保守原则：任何未覆盖形态（builtin 接收、闭环递归、条件中读取、循环内移动、
//! 赋值改写、结构化解构、条件分支路径不对称消费）一律判 false（fail-closed）。
//!
//! 流动模型：`stmts_flow(stmts, live) -> Result<post_live, ()>`——路径敏感；
//! `If` 的 join 要求每个 live 名字在 then/else 两分支后的存活状态一致（两分支
//! 都活 → 仍活；都消名 → 已亡；仅一分支存活 → 路径依赖 → fail-closed），保证
//! join 后的分析对两条运行时路径同时成立。

use crate::ast::{Expr, FuncDef, Item, MatchArm, PatternKind, Stmt, Type};
use crate::core::checker::Checker;

// ─── 表达式 / 语句的"触及"查询（保守，用于 fail-closed 门）───────────

/// 名字是否作为子 place 出现在表达式里（`x`、`x.f`、`x[0]`、容器字面量、元组…）。
fn expr_uses_name(e: &Expr, names: &[String]) -> bool {
    match e.unlocated() {
        Expr::Ident(n) => names.iter().any(|w| w == n),
        Expr::Field(obj, _)
        | Expr::TupleIndex(obj, _)
        | Expr::Index(obj, _)
        | Expr::Unary(crate::ast::UnOp::Deref, obj) => expr_uses_name(obj, names),
        Expr::Call(callee, args) => {
            expr_uses_name(callee, names) || args.iter().any(|a| expr_uses_name(a, names))
        }
        Expr::Tuple(elems) => elems.iter().any(|el| expr_uses_name(el, names)),
        Expr::List(elems) => elems.iter().any(|el| expr_uses_name(el, names)),
        Expr::Binary(_, l, r) => expr_uses_name(l, names) || expr_uses_name(r, names),
        Expr::Record { fields, .. } => fields.iter().any(|f| expr_uses_name(&f.value, names)),
        Expr::Cast(inner, _) | Expr::Try(inner) | Expr::Await(inner) | Expr::Spawn(inner) => {
            expr_uses_name(inner, names)
        }
        Expr::Match(scrutinee, arms) => {
            expr_uses_name(scrutinee, names)
                || arms.iter().any(|a| {
                    a.guard.as_ref().is_some_and(|g| expr_uses_name(g, names))
                        || expr_uses_name(&a.body, names)
                })
        }
        Expr::Lambda { body, .. } => body.iter().any(|s| stmt_uses_name(s, names)),
        Expr::NamedArg(_, value) => expr_uses_name(value, names),
        Expr::Block(stmts) => stmts.iter().any(|s| stmt_uses_name(s, names)),
        Expr::If {
            cond, then_, else_, ..
        } => {
            expr_uses_name(cond, names)
                || then_.iter().any(|s| stmt_uses_name(s, names))
                || else_
                    .as_ref()
                    .is_some_and(|es| es.iter().any(|s| stmt_uses_name(s, names)))
        }
        _ => false,
    }
}

fn stmt_uses_name(s: &Stmt, names: &[String]) -> bool {
    match s.unlocated() {
        Stmt::Let {
            init: Some(e), pat, ..
        } => expr_uses_name(e, names) || pattern_uses_name(pat, names),
        Stmt::Assign { target, value } => {
            expr_uses_name(target, names) || expr_uses_name(value, names)
        }
        Stmt::Return(Some(e)) => expr_uses_name(e, names),
        Stmt::Drop(e) => expr_uses_name(e, names),
        Stmt::Expr(e) => expr_uses_name(e, names),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_uses_name(cond, names)
                || then_.iter().any(|s| stmt_uses_name(s, names))
                || else_
                    .as_ref()
                    .is_some_and(|es| es.iter().any(|s| stmt_uses_name(s, names)))
        }
        Stmt::IfLet {
            pat,
            init,
            then_,
            else_,
            ..
        } => {
            expr_uses_name(init, names)
                || pattern_uses_name(pat, names)
                || then_.iter().any(|s| stmt_uses_name(s, names))
                || else_
                    .as_ref()
                    .is_some_and(|es| es.iter().any(|s| stmt_uses_name(s, names)))
        }
        Stmt::While { cond, body, .. } => {
            expr_uses_name(cond, names) || body.iter().any(|s| stmt_uses_name(s, names))
        }
        Stmt::WhileLet {
            init, body, pat, ..
        } => {
            expr_uses_name(init, names)
                || pattern_uses_name(pat, names)
                || body.iter().any(|s| stmt_uses_name(s, names))
        }
        Stmt::For {
            var,
            iterable,
            body,
            ..
        } => {
            expr_uses_name(iterable, names)
                || pattern_uses_name(var, names)
                || body.iter().any(|s| stmt_uses_name(s, names))
        }
        Stmt::Loop(body) => body.iter().any(|s| stmt_uses_name(s, names)),
        Stmt::Block(block)
        | Stmt::Arena(block)
        | Stmt::Unsafe(block)
        | Stmt::IeeeFloat(block)
        | Stmt::Parasteps(block)
        | Stmt::OnFailure(block)
        | Stmt::Defer(block) => block.iter().any(|s| stmt_uses_name(s, names)),
        Stmt::Func(f) => f.body.iter().any(|s| stmt_uses_name(s, names)),
        _ => false,
    }
}

/// 名字是否以"整体值"位置出现在表达式里：`return x` / `return (x,)` /
/// `return [x]` / record 字面量都是整体转移；`x.f` / `x[0]` / `f(x)` 是投影或
/// 调用实参，不是整体（投影 = 只转移一部分，容器余部静默弃置 → H2 逃逸）。
/// Cast 同样不算（类型改写，线性值不随类型转换保持）——保守排除。
fn expr_whole_contains(e: &Expr, name: &str, checker: &Checker<'_>) -> bool {
    match e.unlocated() {
        Expr::Ident(n) => n == name,
        Expr::Tuple(elems) => elems
            .iter()
            .any(|el| expr_whole_contains(el, name, checker)),
        Expr::List(elems) => elems
            .iter()
            .any(|el| expr_whole_contains(el, name, checker)),
        Expr::Record { fields, .. } => fields
            .iter()
            .any(|f| expr_whole_contains(&f.value, name, checker)),
        Expr::Call(callee, args) => {
            // 构造包装（`Some(x)` / `Ok(v)`）：非函数、非 builtin 的标识符
            // 调用 = 数据构造器（正常 checker 已解析）——实参整体进入构造值，
            // 等价元组字面量的整体转移。真实函数的嵌套调用 = 转移链的一个环节
            // （另由 call_transfer 处理）→ 此处保守 false。
            if let Expr::Ident(n) = callee.unlocated() {
                if !crate::core::builtins::is_builtin_callable(n) && !checker.funcs.contains_key(n)
                {
                    return args.iter().any(|a| expr_whole_contains(a, name, checker));
                }
            }
            false
        }
        _ => false,
    }
}

fn pattern_uses_name(p: &crate::ast::Pattern, names: &[String]) -> bool {
    match &p.kind {
        crate::ast::PatternKind::Variable(n) => names.iter().any(|w| w == n),
        crate::ast::PatternKind::Tuple(ps) | crate::ast::PatternKind::Array(ps) => {
            ps.iter().any(|p| pattern_uses_name(p, names))
        }
        crate::ast::PatternKind::Constructor(_, ps) => {
            ps.iter().any(|(_, p)| pattern_uses_name(p, names))
        }
        // Slice 带 rest 绑定：保守视为可触及（`[a, ..rest]` rest 可能承载值）。
        crate::ast::PatternKind::Slice(ps, rest) => {
            ps.iter().any(|p| pattern_uses_name(p, names))
                || rest.as_ref().is_some_and(|r| pattern_uses_name(r, names))
        }
        _ => false,
    }
}

/// 模式绑定的单一标识符（`let q = x` 纯别名只接受 Variable 模式）。
fn pattern_binds_ident(p: &crate::ast::Pattern) -> Option<String> {
    match &p.kind {
        crate::ast::PatternKind::Variable(n) => Some(n.clone()),
        _ => None,
    }
}

// ─── Checker 接口 ─────────────────────────────────────────────────────

impl<'a> Checker<'a> {
    /// 顶层 `Item::Func` 体查表（stdlib 源也可以命中；找不到 → None，调用方
    /// 保守 fail-closed）。
    pub(crate) fn find_func_def_ast(&self, name: &str) -> Option<&'a FuncDef> {
        self.file.items.iter().find_map(|item| match item {
            Item::Func(f) if f.name == name => Some(f),
            Item::Module(m) => m.items.iter().find_map(|inner| match inner {
                Item::Func(f) if f.name == name => Some(f),
                _ => None,
            }),
            _ => None,
        })
    }

    /// 函数 `name` 第 `param_index` 个参数是否位于泛型位置（其类型提及任一
    /// 泛型参数名）。具体位置恒可信（callee 自身名字级分析追踪），返回 false。
    fn param_is_generic_position(&self, name: &str, param_index: usize) -> bool {
        let Some(generic_names) = self.func_generics.get(name) else {
            return false;
        };
        if generic_names.is_empty() {
            return false;
        }
        let Some(param_ty) = self.funcs.get(name).and_then(|(ps, _)| ps.get(param_index)) else {
            return false;
        };
        generic_names.iter().any(|gp| {
            crate::core::type_folder::type_any(param_ty, &|cand| {
                matches!(
                    cand.unlocated(),
                    crate::ast::Type::Name(n, _) if *n == gp.name
                )
            })
        })
    }

    /// 0.1.9 Phase A: 函数 `name` 中声明为 `linear T` 种类的泛型参数名集合。
    pub(crate) fn linear_kind_generic_names(&self, name: &str) -> Vec<String> {
        self.func_generics
            .get(name)
            .map(|gps| {
                gps.iter()
                    .filter(|g| {
                        matches!(
                            g.kind,
                            crate::ast::GenericKind::Linear | crate::ast::GenericKind::LinearDrop
                        )
                    })
                    .map(|g| g.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 0.39.58 (Phase C): 函数 `name` 第 `param_index` 个参数是否引用某
    /// `linear drop T`（drop-tolerant）种类泛型。`linear drop T` 允许体 drop T，
    /// 但实例化必须可 drop（SessionChan 除外）。
    pub(crate) fn param_uses_linear_drop_kind(&self, name: &str, param_index: usize) -> bool {
        let linear_drop = self
            .func_generics
            .get(name)
            .map(|gps| {
                gps.iter()
                    .filter(|g| g.kind == crate::ast::GenericKind::LinearDrop)
                    .map(|g| g.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if linear_drop.is_empty() {
            return false;
        }
        let Some(param_ty) = self.funcs.get(name).and_then(|(ps, _)| ps.get(param_index)) else {
            return false;
        };
        Self::param_type_refs_linear_kind(param_ty, &linear_drop)
    }

    /// 0.39.59: 参数表面类型是否引用给定线性种类泛型名。与 `type_any` 不同，
    /// **不深入函数类型**（`Type::Func` / `Type::ExternFunc`）：可调用值是
    /// 普通值而非线性资源——`func(T) -> i32` 参数只是签名里提到 T，并不携带
    /// 线性负载，定义时 E0841 不应把这类参数当线性值校验（0.39.59 实证：
    /// `foldT<T>(xs, f: func(T)->i32)` 的 f 被误检）。
    fn param_type_refs_linear_kind(ty: &crate::ast::Type, linear: &[String]) -> bool {
        if matches!(
            ty.unlocated(),
            crate::ast::Type::Func(_, _) | crate::ast::Type::ExternFunc(_, _)
        ) {
            return false;
        }
        crate::core::type_folder::type_any(ty, &|cand| {
            matches!(
                cand.unlocated(),
                crate::ast::Type::Name(n, _) if linear.iter().any(|l| l == n)
            )
        })
    }

    /// 0.1.9 Phase A: 函数 `name` 第 `param_index` 个参数是否引用某 `linear T`
    /// 种类泛型（可经容器 / 元组 / Option 等嵌套，但不深入函数类型）。调用点
    /// 据此对线性实参放行（kind 兼容），定义时据此做 transfer-only 体校验。
    pub(crate) fn param_uses_linear_kind(&self, name: &str, param_index: usize) -> bool {
        let linear = self.linear_kind_generic_names(name);
        if linear.is_empty() {
            return false;
        }
        let Some(param_ty) = self.funcs.get(name).and_then(|(ps, _)| ps.get(param_index)) else {
            return false;
        };
        let r = Self::param_type_refs_linear_kind(param_ty, &linear);
        r
    }

    /// 泛型函数 `name` 的第 `param_index` 个参数是否线性黑盒健全（见模块头）。
    /// `allow_drop=false` 时 drop-从句被禁用（transfer-only：SessionChan 及其
    /// 任意嵌套只能转移，中途 drop = E0425 协议弃置——concrete 语义同款）。
    /// 记忆化（双模式独立缓存）+ 递归守护；具体（非泛型）参数位置直接判 true。
    pub(crate) fn generic_linear_blackbox_sound(
        &mut self,
        name: &str,
        param_index: usize,
        allow_drop: bool,
    ) -> bool {
        if !self.param_is_generic_position(name, param_index) {
            return true;
        }
        {
            let cache = if allow_drop {
                &self.linear_blackbox_cache
            } else {
                &self.linear_blackbox_transfer_cache
            };
            if let Some(cached) = cache.get(name) {
                if let Some(v) = cached.get(param_index) {
                    return *v;
                }
            }
        }
        let guard = format!("{name}#{param_index}#{}", if allow_drop { 1 } else { 0 });
        if !self.linear_blackbox_visiting.insert(guard.clone()) {
            // 递归闭环 → 无法证明 → fail-closed。
            return false;
        }
        let Some(func) = self.find_func_def_ast(name) else {
            self.linear_blackbox_visiting.remove(&guard);
            return false;
        };
        let Some(param) = func.params.get(param_index) else {
            self.linear_blackbox_visiting.remove(&guard);
            return false;
        };
        // 0.36.42: if-let 穷举消解只对 Option 中介面开（None 零负载 =
        // 具体面 0.36.36 义务消解镜像）；List/Result/自定义枚举保持 fail-closed
        // （concrete E0256 同款）。入口按参数表面类型设置，供 Stmt::IfLet 读。
        let saved_scrutinee_option = self.blackbox_param_scrutinee_option;
        let is_option_scrutinee = match param.ty.unlocated() {
            Type::Name(n, _) => n == "Option",
            Type::Option(_) => true,
            _ => false,
        };
        self.blackbox_param_scrutinee_option = is_option_scrutinee;
        // 0.36.45: 元素 Option-ness 链（List<Option<T>> → [true]；
        // List<List<Option<T>>> → [false, true]）。For 臂逐层弹出。
        // 链与当前元素标志均按旧值存取：转移调用会重入此入口（then 块内
        // 的泛型调用 → call_transfer → 目标函数的 bb-sound），硬置 None
        // 会 clobber 外层 for 的元素标志。
        let saved_chain = std::mem::take(&mut self.blackbox_element_option_chain);
        let saved_element = self.blackbox_current_element_option;
        self.blackbox_element_option_chain = option_element_chain(param.ty.unlocated());
        let ok = linear_blackbox_body(
            &func.body,
            std::slice::from_ref(&param.name),
            allow_drop,
            self,
        );
        self.blackbox_param_scrutinee_option = saved_scrutinee_option;
        self.blackbox_element_option_chain = saved_chain;
        self.blackbox_current_element_option = saved_element;
        self.linear_blackbox_visiting.remove(&guard);
        let cache = if allow_drop {
            &mut self.linear_blackbox_cache
        } else {
            &mut self.linear_blackbox_transfer_cache
        };
        let entry = cache.entry(name.to_string()).or_default();
        while entry.len() <= param_index {
            entry.push(false);
        }
        entry[param_index] = ok;
        ok
    }

    /// 0.39.61: 定义时对给定 AST 参数 + 体直接做线性黑盒 body 分析（E0841 用）。
    /// 与 `generic_linear_blackbox_sound` 不同：不经过 `find_func_def_ast`
    /// （仅命中顶层函数），因此顶层函数与 impl/actor 方法共用。自递归信任
    /// （BLACKBOX-REC-001）在 `call_transfer` 内，不依赖此处。
    pub(crate) fn linear_kind_body_sound(
        &mut self,
        param: &crate::ast::Param,
        body: &[crate::ast::Stmt],
        allow_drop: bool,
    ) -> bool {
        let saved_scrutinee_option = self.blackbox_param_scrutinee_option;
        let is_option_scrutinee = match param.ty.unlocated() {
            crate::ast::Type::Name(n, _) => n == "Option",
            crate::ast::Type::Option(_) => true,
            _ => false,
        };
        self.blackbox_param_scrutinee_option = is_option_scrutinee;
        let saved_chain = std::mem::take(&mut self.blackbox_element_option_chain);
        let saved_element = self.blackbox_current_element_option;
        self.blackbox_element_option_chain = option_element_chain(param.ty.unlocated());
        let ok = linear_blackbox_body(body, std::slice::from_ref(&param.name), allow_drop, self);
        self.blackbox_param_scrutinee_option = saved_scrutinee_option;
        self.blackbox_element_option_chain = saved_chain;
        self.blackbox_current_element_option = saved_element;
        ok
    }

    /// 表层类型是否携带 SessionChan（递归容器/元组/Option/Result 等）。
    /// 会话端点只能转移不能 drop（concrete 面 E0425）→ 泛型直通须走
    /// transfer-only 模式（allow_drop=false）。
    pub(crate) fn surface_type_contains_session(&self, ty: &crate::ast::Type) -> bool {
        match ty.unlocated() {
            crate::ast::Type::Name(name, args) => {
                (name == "SessionChan" || name == "session_chan")
                    || args.iter().any(|a| self.surface_type_contains_session(a))
            }
            crate::ast::Type::Option(inner)
            | crate::ast::Type::CBuffer(inner)
            | crate::ast::Type::Slice(inner)
            | crate::ast::Type::Newtype(_, inner) => self.surface_type_contains_session(inner),
            crate::ast::Type::Array(inner, _) => self.surface_type_contains_session(inner),
            crate::ast::Type::Result(ok, err) => {
                self.surface_type_contains_session(ok) || self.surface_type_contains_session(err)
            }
            crate::ast::Type::Tuple(items) => items
                .iter()
                .any(|item| self.surface_type_contains_session(item)),
            crate::ast::Type::Located { ty, .. } => self.surface_type_contains_session(ty),
            _ => false,
        }
    }
}

// ─── 路径敏感流动分析 ─────────────────────────────────────────────────

/// 流动状态：`live` = 仍携带义务的名字；`consumed` = 已转移/已 drop/已被别名
/// 接管的名字（之后任何再触 = use-after-move → fail-closed——黑盒直通只承认
/// 纯转移，不承认"转移后复用"的半黑盒）。
#[derive(Clone, Default)]
struct FlowState {
    live: Vec<String>,
    consumed: Vec<String>,
}

impl FlowState {
    fn any_uses(&self, e: &Expr) -> bool {
        expr_uses_name(e, &self.live) || expr_uses_name(e, &self.consumed)
    }
    fn any_uses_stmt(&self, s: &Stmt) -> bool {
        stmt_uses_name(s, &self.live) || stmt_uses_name(s, &self.consumed)
    }
    /// 转移/drop/别名接管 `name`（从 live 移入 consumed）。
    fn consume(&mut self, name: &str) {
        self.live.retain(|w| w != name);
        if !self.consumed.iter().any(|w| w == name) {
            self.consumed.push(name.to_string());
        }
    }
}

/// 判定函数体在参数 `live_in`（单元素：被检参数名）下线性黑盒健全：每条出口
/// 路径都转移（或 `allow_drop` 时显式 drop）该名字；任何静默弃值 / 转移后复用
/// → false。
pub(crate) fn linear_blackbox_body(
    stmts: &[Stmt],
    live_in: &[String],
    allow_drop: bool,
    checker: &mut Checker<'_>,
) -> bool {
    let start = FlowState {
        live: live_in.to_vec(),
        consumed: Vec::new(),
    };
    stmts_flow(stmts, start, allow_drop, checker).is_ok_and(|post| post.live.is_empty())
}

/// 顺序语句流动：`Ok(post_state)` / `Err(())`（某路径静默弃值、转移后复用或
/// 未覆盖形态）。每条语句的 post-state 是下一条语句的输入。
/// 模式绑定的名字收集 + 弃置标记（切片 2 结构化整体消费用）。
/// - `Variable` 叶 = 绑定名（臂体/循环体必须黑盒处理）；
/// - `Wildcard` / Slice `..rest` 通配 = 弃置对应份额 → `drops`（由 allow_drop 门禁）；
/// - `Literal` 叶 = 常量成分（恒非线性）→ 忽略；
/// - 其余形态（未知/嵌套 err）。
fn pattern_binding_info(pk: &PatternKind) -> Result<PatternInfo, ()> {
    let mut info = PatternInfo {
        names: Vec::new(),
        drops: false,
    };
    collect_pattern(pk, &mut info)?;
    Ok(info)
}

fn collect_pattern(pk: &PatternKind, info: &mut PatternInfo) -> Result<(), ()> {
    match pk {
        PatternKind::Variable(name) => {
            if name == "None" {
                // 零参构造（unit variant）在模式面解析为裸标识符——`None`
                // 不承载值，无绑定无弃置（正常 checker 已做穷举/变体解析）。
                return Ok(());
            }
            info.names.push(name.clone());
            Ok(())
        }
        PatternKind::Wildcard => {
            info.drops = true;
            Ok(())
        }
        PatternKind::Literal(_) => Ok(()), // 常量成分恒非线性
        PatternKind::Constructor(_, fields) => {
            for (_, p) in fields {
                collect_pattern(&p.kind, info)?;
            }
            Ok(())
        }
        PatternKind::Tuple(ps) | PatternKind::Array(ps) => {
            for p in ps {
                collect_pattern(&p.kind, info)?;
            }
            Ok(())
        }
        PatternKind::Slice(ps, rest) => {
            for p in ps {
                collect_pattern(&p.kind, info)?;
            }
            if let Some(r) = rest {
                collect_pattern(&r.kind, info)?;
            }
            Ok(())
        }
    }
}

struct PatternInfo {
    names: Vec<String>,
    drops: bool,
}

fn expr_tail_flow(
    e: &Expr,
    state: FlowState,
    allow_drop: bool,
    checker: &mut Checker<'_>,
) -> Result<FlowState, ()> {
    if state
        .consumed
        .iter()
        .any(|n| expr_uses_name(e, std::slice::from_ref(n)))
    {
        return Err(()); // 转移后复用
    }
    match e.unlocated() {
        Expr::Call(callee, args) => {
            // 尾部调用链：`{ g(x) }` / `return g(x)` —— x 经 g 转移。
            let mut t = state;
            let _ = call_transfer(callee, args, &mut t, allow_drop, checker)?;
            if !t.live.is_empty() {
                return Err(()); // 有名字未移入调用 → 弃值
            }
            Ok(t)
        }
        Expr::Match(scrutinee, arms) => {
            let post = match_flow(scrutinee, arms, state, allow_drop, checker)?;
            if !post.live.is_empty() {
                return Err(());
            }
            Ok(post)
        }
        Expr::Block(stmts) => {
            // 块表达式尾：`{ let ...; x }` —— 内部流动即尾表达式流动。
            let post = stmts_flow(stmts, state, allow_drop, checker)?;
            if !post.live.is_empty() {
                return Err(());
            }
            Ok(post)
        }
        _ => {
            for name in &state.live {
                if !expr_whole_contains(e, name, checker) {
                    return Err(());
                }
            }
            let mut next = state;
            next.live.clear(); // 整体随返回值转移
            Ok(next)
        }
    }
}

/// 穷举解构（0.36.36-37 容器义务消解语义的泛型面）：scrutinee 必须整体包含
/// 恰一个 live 名；每个臂的绑定名在臂体内黑盒处理（尾表达式流）；无绑定臂
/// （wildcard/.. 弃置）由 allow_drop 门禁；臂体/guard 不得再触 scrutinee。
fn match_flow(
    scrutinee: &Expr,
    arms: &[MatchArm],
    state: FlowState,
    allow_drop: bool,
    checker: &mut Checker<'_>,
) -> Result<FlowState, ()> {
    if !state.any_uses(scrutinee) && !arms.iter().any(|a| expr_uses_name(&a.body, &state.live)) {
        return Ok(state); // 未触碰任何 live 名
    }
    let candidates: Vec<String> = state
        .live
        .iter()
        .filter(|n| expr_whole_contains(scrutinee, n, checker))
        .cloned()
        .collect();
    if candidates.len() != 1 {
        return Err(());
    }
    let v = candidates[0].clone();
    if state
        .live
        .iter()
        .any(|n| n != &v && expr_uses_name(scrutinee, std::slice::from_ref(n)))
    {
        return Err(()); // 多个 live 名进入同一 scrutinee → 保守
    }
    for arm in arms {
        if let Some(g) = &arm.guard {
            if expr_uses_name(g, std::slice::from_ref(&v)) {
                return Err(()); // 线性值进 guard → fail-closed
            }
        }
        if expr_uses_name(&arm.body, std::slice::from_ref(&v)) {
            return Err(()); // 臂体再触 v（穷举解构后 v 已消解）
        }
        let info = pattern_binding_info(&arm.pat.kind)?;
        if info.drops && !allow_drop {
            return Err(()); // `_` 弃置余部 = 协议弃置（transfer-only）
        }
        if !info.names.is_empty() {
            let st = FlowState {
                live: info.names.clone(),
                consumed: Vec::new(),
            };
            let post = expr_tail_flow(&arm.body, st, allow_drop, checker)?;
            if !post.live.is_empty() {
                return Err(()); // 臂体未完全处理绑定 → 弃值
            }
        }
    }
    let mut next = state;
    next.consume(&v);
    Ok(next)
}

fn stmts_flow(
    stmts: &[Stmt],
    state_in: FlowState,
    allow_drop: bool,
    checker: &mut Checker<'_>,
) -> Result<FlowState, ()> {
    let mut state = state_in;
    for (idx, stmt) in stmts.iter().enumerate() {
        // 块尾裸表达式 = 返回（`func f<T>(x: T) -> T { x }` 的体是
        // `[Expr(x)]`，等价 `return x`——正常 checker 同样按尾表达式处理）。
        if idx == stmts.len() - 1 {
            if let Stmt::Expr(e) = stmt.unlocated() {
                return expr_tail_flow(e, state, allow_drop, checker);
            }
        }
        state = stmt_flow(stmt, state, allow_drop, checker)?;
        if state.live.is_empty() {
            // 0.36.45: live 清空后的剩余语句仍须检查已消费名字的复用——
            // `sink_g(y); sink_g(y)` 的第二条（y 已转移再触 = 转移后复用）。
            // 此前直接返回使尾部/后续语句脱离检查（泛型 double-use 漏网；
            // concrete 面由 dataflow 的 Move-after-Consumed 拒绝）。
            for rest in &stmts[idx + 1..] {
                if state
                    .consumed
                    .iter()
                    .any(|n| stmt_uses_name(rest, std::slice::from_ref(n)))
                {
                    return Err(());
                }
            }
            return Ok(state);
        }
    }
    Ok(state)
}

/// 单语句流动（返回该语句后的状态）。`Err(())` = 该语句形态未覆盖 /
/// 已构成弃值 / 转移后复用。
fn stmt_flow(
    s: &Stmt,
    state: FlowState,
    allow_drop: bool,
    checker: &mut Checker<'_>,
) -> Result<FlowState, ()> {
    match s.unlocated() {
        Stmt::Let {
            init: Some(e), pat, ..
        } => {
            // 切片 5：闭包绑定的线性义务在定义点结算——后续调用只传闭包标识符
            // （`mapT(xs, c)`），那时无法再检查体；弃置参数体在此同款拒绝。
            if let Expr::Lambda { params, body, .. } = e.unlocated() {
                let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                if !linear_blackbox_body(body, &param_names, allow_drop, checker) {
                    return Err(());
                }
            }
            if !state.any_uses(e) {
                return Ok(state);
            }
            if state
                .consumed
                .iter()
                .any(|n| expr_uses_name(e, std::slice::from_ref(n)))
            {
                return Err(()); // 转移后复用
            }
            // 纯别名：`let q = x`（且 x 在 live 中）→ 义务转移到 q。
            if let (Some(bound), Expr::Ident(src)) = (pattern_binds_ident(pat), e.unlocated()) {
                if state.live.iter().any(|w| w == src) {
                    let mut next = state;
                    next.consume(src);
                    next.live.push(bound);
                    return Ok(next);
                }
            }
            // 调用初始化：`let y = g(x)` —— 移交调用转移逻辑；返回值是否携带
            // 线性值由接收者 transfer-模式判定（调体每条路径 return 该参数）。
            if let Expr::Call(callee, args) = e.unlocated() {
                let mut next = state;
                let returns_value = call_transfer(callee, args, &mut next, allow_drop, checker)?;
                if returns_value {
                    if let Some(bound) = pattern_binds_ident(pat) {
                        next.live.push(bound); // y 承载转移出的值
                    } else {
                        return Err(()); // 结构化解构返回值 = 消费形状 → 切片 2
                    }
                }
                return Ok(next);
            }
            // 其他表达式包含 live 名字（`let q = x + 1` 等）→ fail-closed。
            Err(())
        }
        Stmt::Assign { target, value } => {
            if state.any_uses(target) {
                return Err(()); // 把线性值写进新槽位 = 改写/移动 → fail-closed
            }
            if state
                .consumed
                .iter()
                .any(|n| expr_uses_name(value, std::slice::from_ref(n)))
            {
                return Err(()); // 转移后复用
            }
            if !state.any_uses(value) {
                return Ok(state);
            }
            // 直接调用：`d = g(x)` —— value 整体移交调用转移逻辑。
            if let Expr::Call(callee, args) = value.unlocated() {
                let mut next = state;
                let _ = call_transfer(callee, args, &mut next, allow_drop, checker)?;
                return Ok(next);
            }
            // 二元累加槽位：`n = n + sink_g(x)` / `n = n + match x { .. }`
            // （for 体计数等）——一条边是"转移表达式"（Call / Match）、另一条
            // 边是纯数据（不得触碰 live）。
            if let Expr::Binary(_, left, right) = value.unlocated() {
                for side in [left, right] {
                    let other = if std::ptr::eq(side, left) {
                        right
                    } else {
                        left
                    };
                    if !state.any_uses(other) {
                        match side.unlocated() {
                            Expr::Call(callee, args) => {
                                let mut next = state;
                                let _ =
                                    call_transfer(callee, args, &mut next, allow_drop, checker)?;
                                return Ok(next);
                            }
                            Expr::Match(scrutinee, arms) => {
                                let post = match_flow(scrutinee, arms, state, allow_drop, checker)?;
                                return Ok(post);
                            }
                            _ => continue,
                        }
                    }
                }
            }
            Err(()) // 其余含 live 的赋值形态 → fail-closed
        }
        Stmt::Return(Some(e)) => {
            // 路径终止：所有 live 名字经尾表达式流转移（整体包含 / 调用链 /
            // 穷举解构 / 块表达式）。
            expr_tail_flow(e, state, allow_drop, checker)
        }
        Stmt::Return(None) => {
            if state.live.is_empty() {
                Ok(state)
            } else {
                Err(()) // 隐式返回值不含任何 live 名字 → 弃值
            }
        }
        Stmt::Drop(e) => {
            if state
                .consumed
                .iter()
                .any(|n| expr_uses_name(e, std::slice::from_ref(n)))
            {
                return Err(()); // 重复 drop / 转移后 drop
            }
            let touched: Vec<String> = state
                .live
                .iter()
                .filter(|n| expr_uses_name(e, std::slice::from_ref(n)))
                .cloned()
                .collect();
            if !touched.is_empty() && !allow_drop {
                return Err(()); // transfer-only：值只许转移，drop = 协议弃置
            }
            // 非整体触碰（drop(xs[0]) 只释放一个元素，余部弃置）→ fail-closed。
            if state.live.iter().any(|n| {
                expr_uses_name(e, std::slice::from_ref(n)) && !expr_whole_contains(e, n, checker)
            }) {
                return Err(());
            }
            let mut next = state;
            for n in touched {
                next.consume(&n);
            }
            Ok(next)
        }
        Stmt::Expr(e) => expr_flow(e, state, allow_drop, checker),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            if state.any_uses(cond) {
                return Err(()); // 线性值进条件 = 读取 / 复用 → fail-closed
            }
            let then_post = stmts_flow(then_, state.clone(), allow_drop, checker)?;
            let else_post = match else_ {
                Some(es) => stmts_flow(es, state.clone(), allow_drop, checker)?,
                None => state.clone(), // 无 else：恒等分支
            };
            // join：每个名字在 then/else 后的存活状态必须一致；每个被消费名
            // 的消费位置同样必须一致（单分支消费 = 路径依赖 → fail-closed）。
            let mut post = FlowState::default();
            for n in &state.live {
                let alive_then = then_post.live.iter().any(|w| w == n);
                let alive_else = else_post.live.iter().any(|w| w == n);
                if alive_then != alive_else {
                    return Err(()); // 路径依赖存活 → 延续无法静态证明 → fail-closed
                }
                let consumed_then = then_post.consumed.iter().any(|w| w == n);
                let consumed_else = else_post.consumed.iter().any(|w| w == n);
                if alive_then && (consumed_then || consumed_else) {
                    // 同分支既消费又存活不可能（consume 移出 live）——防御。
                    return Err(());
                }
                if alive_then {
                    post.live.push(n.clone());
                } else {
                    post.consume(n);
                }
            }
            // 分支内消费的既有消费集（不以 live 起始的名字不可能被消费）。
            for n in &state.consumed {
                let c_then = then_post.consumed.iter().any(|w| w == n);
                let c_else = else_post.consumed.iter().any(|w| w == n);
                if c_then != c_else {
                    return Err(()); // 消费路径不一致 → 延续复用无法证明 → fail-closed
                }
                if c_then {
                    post.consume(n);
                }
            }
            Ok(post)
        }
        Stmt::IfLet {
            pat,
            init,
            then_,
            else_,
        } => {
            if !state.any_uses(init) {
                return Ok(state);
            }
            if state
                .consumed
                .iter()
                .any(|n| expr_uses_name(init, std::slice::from_ref(n)))
            {
                return Err(()); // 转移后复用
            }
            // scrutinee 必须整体包含恰一个 live 名（0.36.36 容器义务解消的
            // 泛型镜像；`foo(o)`/`o.f` 等投影位置 → 非整体 → fail-closed）。
            let candidates: Vec<String> = state
                .live
                .iter()
                .filter(|n| expr_whole_contains(init, n, checker))
                .cloned()
                .collect();
            if candidates.len() != 1 {
                return Err(());
            }
            let v = candidates[0].clone();
            if state
                .live
                .iter()
                .any(|n| n != &v && expr_uses_name(init, std::slice::from_ref(n)))
            {
                return Err(());
            }
            let info = pattern_binding_info(&pat.kind)?;
            // 容器经 if-let 消解后，then/else 块内不得再触容器名。
            if then_
                .iter()
                .any(|st| stmt_uses_name(st, std::slice::from_ref(&v)))
                || else_.as_ref().is_some_and(|es| {
                    es.iter()
                        .any(|st| stmt_uses_name(st, std::slice::from_ref(&v)))
                })
            {
                return Err(());
            }
            // then 块：绑定名黑盒流动，必须完全处理（臂内弃置 = 具体面
            // E0256 同款禁令；`if let Some(x) = o { sink_g(x) }` 恰一次）。
            let mut then_state = FlowState::default();
            for n in &info.names {
                then_state.live.push(n.clone());
            }
            let then_post = stmts_flow(then_, then_state, allow_drop, checker)?;
            if !then_post.live.is_empty() {
                return Err(()); // 绑定名未完全处理 → 弃值
            }
            // 零绑定模式（`if let _ = o`）：整个容器弃置 → drop 门禁。
            if info.names.is_empty() && !allow_drop {
                return Err(());
            }
            // else（及 no-else 回退）：仅 Option 中介面开——None 变体零负载，
            // 不匹配路径不涉及任何弃置 → 无 drop 门禁（transfer-only 的会话
            // 也可 if-let 转移）；其余容器 fail-closed（concrete E0256）。
            if !checker.blackbox_param_scrutinee_option
                && checker.blackbox_current_element_option != Some(true)
            {
                return Err(());
            }
            if let Some(es) = else_ {
                let else_post = stmts_flow(es, FlowState::default(), allow_drop, checker)?;
                if !else_post.live.is_empty() {
                    return Err(());
                }
            }
            let mut post = state;
            post.consume(&v);
            Ok(post)
        }
        Stmt::While { .. } | Stmt::WhileLet { .. } | Stmt::Loop(_) => {
            if state.any_uses_stmt(s) {
                Err(()) // 循环 0 次迭代 + 路径敏感消费不可证 → fail-closed
            } else {
                Ok(state)
            }
        }
        Stmt::For {
            var,
            iterable,
            body,
        } => {
            if !state.any_uses_stmt(s) {
                return Ok(state);
            }
            if state
                .consumed
                .iter()
                .any(|n| stmt_uses_name(s, std::slice::from_ref(n)))
            {
                return Err(()); // 转移后复用
            }
            // 穷举逐元素解构（0.36.37 容器义务消解的泛型面）：iterable 必须
            // 整体包含恰一个 live 名；var 模式收集的元素绑定在循环体内黑盒
            // 处理（0.36.37 for = 每元素恰一次）；循环体不得再触容器名。
            let candidates: Vec<String> = state
                .live
                .iter()
                .filter(|n| expr_whole_contains(iterable, n, checker))
                .cloned()
                .collect();
            if candidates.len() != 1 {
                return Err(());
            }
            let v = candidates[0].clone();
            if state
                .live
                .iter()
                .any(|n| n != &v && expr_uses_name(iterable, std::slice::from_ref(n)))
            {
                return Err(()); // 多个 live 名进入同一 iterable → 保守
            }
            let info = pattern_binding_info(&var.kind)?;
            if info.drops && !allow_drop {
                return Err(()); // `for _ in v` 逐元素弃置 = 协议弃置
            }
            if body
                .iter()
                .any(|st| stmt_uses_name(st, std::slice::from_ref(&v)))
            {
                return Err(()); // 循环体内再触容器名 → fail-closed
            }
            if !info.names.is_empty() {
                // 0.36.45: 弹出本层元素 Option-ness（无参类型链则 None
                // → 体颠的 if-let 保持 fail-closed）；嵌套 for 依链逐层。
                let saved_chain = std::mem::take(&mut checker.blackbox_element_option_chain);
                let saved_element = checker.blackbox_current_element_option;
                let mut chain = saved_chain;
                checker.blackbox_current_element_option = chain.pop();
                let ok = linear_blackbox_body(body, &info.names, allow_drop, checker);
                checker.blackbox_element_option_chain = chain;
                checker.blackbox_current_element_option = saved_element;
                if !ok {
                    return Err(()); // 元素绑定未在黑盒规则内处理
                }
            }
            let mut next = state;
            next.consume(&v);
            Ok(next)
        }
        Stmt::Block(block)
        | Stmt::Arena(block)
        | Stmt::Unsafe(block)
        | Stmt::IeeeFloat(block)
        | Stmt::Parasteps(block)
        | Stmt::OnFailure(block)
        | Stmt::Defer(block) => stmts_flow(block, state, allow_drop, checker),
        Stmt::Func(_) => Ok(state),
        _ => Ok(state),
    }
}

/// 表达式级流动：非调用表达式触碰 live/consumed → fail-closed；调用移交
/// `call_transfer`。
fn expr_flow(
    e: &Expr,
    state: FlowState,
    allow_drop: bool,
    checker: &mut Checker<'_>,
) -> Result<FlowState, ()> {
    match e.unlocated() {
        Expr::Call(callee, args) => {
            let mut next = state;
            let _ = call_transfer(callee, args, &mut next, allow_drop, checker)?;
            Ok(next)
        }
        Expr::Match(scrutinee, arms) => match_flow(scrutinee, arms, state, allow_drop, checker),
        _ => {
            if state.any_uses(e) {
                Err(())
            } else {
                Ok(state)
            }
        }
    }
}

/// 调用 = 向"线性安全接收者"转移。返回 `Ok(true)` 当返回值携带被转移的值
/// （接收者 transfer-模式：调体每条路径 return 该参数）。
/// 整体转移-out：每个 live 名必须作为恰一个实参的"完整反应"进入该调用
/// （构造包装 / 方法实参 / 可调用值调用共用）。实参本身可以是转移链
/// （`Some(attach(x))` / `out.push(f(x))` 的内层调用）→ 递归处理；
/// 返回值携带被包装的值（构造面）/ 由具体面在各自 site 追踪（可调用面）。
fn transfer_wrapped_args(
    args: &[Expr],
    state: &mut FlowState,
    allow_drop: bool,
    checker: &mut Checker<'_>,
) -> Result<bool, ()> {
    let mut wrapped_any = false;
    for n in state.live.clone() {
        let matching: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| expr_uses_name(a, std::slice::from_ref(&n)))
            .map(|(i, _)| i)
            .collect();
        if matching.len() != 1 {
            return Err(()); // 未进入 / 多实参 → 弃值或重复借用
        }
        let arg = &args[matching[0]];
        let mut sub = FlowState {
            live: vec![n.clone()],
            consumed: Vec::new(),
        };
        match arg.unlocated() {
            Expr::Call(c2, a2) => {
                let _ = call_transfer(c2, a2, &mut sub, allow_drop, checker)?;
            }
            Expr::Match(s2, arms2) => {
                sub = match_flow(s2, arms2, sub, allow_drop, checker)?;
            }
            _ => {
                if !expr_whole_contains(arg, &n, checker) {
                    return Err(());
                }
                sub.live.clear();
            }
        }
        if !sub.live.is_empty() {
            return Err(()); // 实参未完全反应 → 弃值
        }
        state.consume(&n);
        wrapped_any = true;
    }
    Ok(wrapped_any)
}

fn call_transfer(
    callee: &Expr,
    args: &[Expr],
    state: &mut FlowState,
    allow_drop: bool,
    checker: &mut Checker<'_>,
) -> Result<bool, ()> {
    if !expr_uses_name(callee, &state.live)
        && !args.iter().any(|a| expr_uses_name(a, &state.live))
        && !state.consumed.iter().any(|n| {
            expr_uses_name(callee, std::slice::from_ref(n))
                || args
                    .iter()
                    .any(|a| expr_uses_name(a, std::slice::from_ref(n)))
        })
    {
        return Ok(false);
    }
    // 已消费名字出现在本次调用（转移后复用）→ fail-closed（在转移处理
    // 之前检查：本调用即将消费的名字不算复用）。
    for n in &state.consumed {
        if expr_uses_name(callee, std::slice::from_ref(n))
            || args
                .iter()
                .any(|a| expr_uses_name(a, std::slice::from_ref(n)))
        {
            return Err(());
        }
    }
    // Lambda 字面量实参 = 匿名"臂"（切片 5 高阶直通）：参数名逐一 live 黑盒
    // 结算体——每次出现都检查（不管实参本身是否触碰 live 名字）；弃置参数
    // 体（`fn(x: T) { 0 }`）在具体面 = 元素泄漏 → 泛型面同款拒绝。
    for arg in args {
        if let Expr::Lambda { params, body, .. } = arg.unlocated() {
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            if !linear_blackbox_body(body, &param_names, allow_drop, checker) {
                return Err(());
            }
        }
    }
    // 方法调用：`receiver.method(args)`（解析为 Call(Field(receiver, _), args)）。
    // 接收者 ∈ live / 已消费 → 线性接收者方法面未开（容器方法 = 余面）→
    // fail-closed；接收者非线性 → 实参中触碰 live 的名字逐一带整体转移。
    if let Expr::Field(receiver, _) = callee.unlocated() {
        if expr_uses_name(receiver, &state.live)
            || state
                .consumed
                .iter()
                .any(|n| expr_uses_name(receiver, std::slice::from_ref(n)))
        {
            return Err(());
        }
        return transfer_wrapped_args(args, state, allow_drop, checker);
    }
    let Expr::Ident(callee_name) = callee.unlocated() else {
        return Err(());
    };
    if crate::core::builtins::is_builtin_callable(callee_name) {
        return Err(()); // builtin 非线性感知 → fail-closed
    }
    let Some(param_tys) = checker.funcs.get(callee_name).cloned() else {
        // 数据构造器包装（`Some(x)` / `Ok(v)`）/ 可调用值调用（`f(x)`）：
        // 每个 live 名必须作为恰一个实参的"完整反应"进入——实参本身可以是
        // 转移链（`Some(attach(x))`）→ 递归处理；构造包装的返回值携带被包装
        // 的值，可调用值的返回值由具体面在各目的 site 追踪。
        return transfer_wrapped_args(args, state, allow_drop, checker);
    };
    // 每个 live 名字必须作为实参移入可信接收者：
    //   - 具体参数位置：callee 自身名字级分析全权追踪 → 恒可信（但返回值
    //     是否携带无法确证 → 保守不算 transfer-out）；
    //   - 泛型参数位置：递归黑盒判定（记忆化 + 递归守护）。
    let mut returns_value = false;
    for n in state.live.clone() {
        let Some((arg_index, _)) = args
            .iter()
            .enumerate()
            .find(|(_, a)| expr_whole_contains(a, &n, checker))
        else {
            if expr_uses_name(callee, std::slice::from_ref(&n)) {
                return Err(());
            }
            return Err(()); // 未移入任何实参 → 弃值
        };
        // 该名字出现在多个实参里（重复借用/多重转移）→ fail-closed。
        let occurrences = args
            .iter()
            .filter(|a| expr_whole_contains(a, &n, checker))
            .count();
        if occurrences > 1 {
            return Err(());
        }
        if param_tys.0.get(arg_index).is_none() {
            return Err(());
        }
        let transfer_out = if checker.param_is_generic_position(callee_name, arg_index) {
            // BLACKBOX-REC-001（0.39.60 关闭）：自递归——若当前正在分析
            // `callee_name` 自身（visiting 守卫含其前缀），则递归调用把 T
            // 委托给同一函数。基例（非递归路径）仍由外层分析强制消费，故
            // 按归纳健全：递归分支不再 fail-closed，且视为 transfer-out。
            let self_recursive = checker
                .linear_blackbox_visiting
                .iter()
                .any(|g| g.starts_with(&format!("{callee_name}#")));
            if !self_recursive
                && !checker.generic_linear_blackbox_sound(callee_name, arg_index, allow_drop)
            {
                return Err(());
            }
            if self_recursive {
                true
            } else {
                // transfer-模式（return-only）独立判定：调体每条路径 return 该参数
                // → 返回值携带该值；否则（drop 路径）返回值不视为携带（保守，
                // 调用方按 concrete 类型照常追踪 y 的线性义务）。
                checker.generic_linear_blackbox_sound(callee_name, arg_index, false)
            }
        } else {
            false // 具体位置：无法确证返回值携带 → 保守
        };
        if transfer_out {
            returns_value = true;
        }
        state.consume(&n);
    }
    Ok(returns_value)
}

/// 0.36.45: 从参数表面类型提取 List 元素 Option-ness 链——
/// `List<Option<T>>` → [true]; `List<List<Option<T>>>` → [false, true];
/// 非 List 或非单元素容器 → 空链（对应 for 层保持 fail-closed）。
fn option_element_chain(ty: &Type) -> Vec<bool> {
    let mut chain = Vec::new();
    let mut current = ty;
    while let Type::Name(name, args) = current.unlocated() {
        if name != "List" || args.len() != 1 {
            break;
        }
        let elem = &args[0];
        let is_option = match elem.unlocated() {
            Type::Name(n, _) => n == "Option",
            Type::Option(_) => true,
            _ => false,
        };
        chain.push(is_option);
        current = elem;
    }
    chain
}
