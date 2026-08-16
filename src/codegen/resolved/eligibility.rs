use crate::core::{
    CheckedConversionKind, CheckedProgram, NodeId, Origin, PrimitiveType, ResolvedBlock,
    ResolvedCallable, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedPattern,
    ResolvedPatternKind, ResolvedPlace, ResolvedStmtKind, ResolvedType, ResolvedTypeId,
};

/// Structured per-function resolved/legacy dispatch statistics (0.34.40,
/// AF-4 前置 1 度量门禁). Emitted as JSON when `MIMI_STAT=1`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DispatchStats {
    /// Entry program name (basename, for correlation).
    pub program: String,
    /// Total non-comptime functions in the program.
    pub total_functions: usize,
    /// Functions compiled through the resolved native emitter.
    pub eligible: usize,
    /// Functions left to the legacy emitter (eligibility skip).
    pub legacy_fallback: usize,
    /// Eligible functions whose emission failed at codegen time
    /// (verify/emit error → legacy recompile).
    pub emit_failed: usize,
    /// Skip-reason histogram (reason string → count).
    pub skip_reasons: std::collections::BTreeMap<String, usize>,
}

impl DispatchStats {
    fn record_skip(&mut self, reason: impl Into<String>) {
        let key = normalize_skip_reason(&reason.into());
        *self.skip_reasons.entry(key).or_insert(0) += 1;
        self.legacy_fallback += 1;
    }
}

/// 0.34.40: Normalize a skip reason into a stable histogram key.
///
/// Eligibility rejection messages from `require_expr` / `require_block` embed
/// full `Debug` renderings of `ResolvedExpr` / `ResolvedCall` (including
/// `@external:<hash>` source identities and `NodeId(...)` internals). Those
/// are unstable across sessions/commits and would make the fallback-rate
/// baseline gate flaky + bloat the JSON. Collapse any reason carrying such
/// unstable markers into a coarse, stable category keyed by the leading noun.
fn normalize_skip_reason(reason: &str) -> String {
    let unstable = reason.contains("NodeId(")
        || reason.contains("@external:")
        || reason.contains("ResolvedCall {")
        || reason.contains("ResolvedExpr {")
        || reason.contains("ResolvedPattern {")
        || reason.contains("ResolvedTypeId(")
        || reason.contains("NominalTypeId(")
        || reason.contains("source_id: SourceId(");
    if !unstable {
        return reason.to_string();
    }
    if reason.starts_with("expression ") || reason.starts_with("try inner") {
        return "unsupported expression".to_string();
    }
    if reason.starts_with("statement ") {
        return "unsupported statement".to_string();
    }
    if reason.starts_with("pattern ") {
        return "unsupported pattern".to_string();
    }
    if reason.starts_with("type ") {
        return "unsupported type".to_string();
    }
    if reason.starts_with("callee ") {
        return "unsupported callee".to_string();
    }
    "unsupported node".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UnsupportedResolvedNode {
    pub owner: NodeId,
    pub node: NodeId,
    pub reason: String,
}

impl UnsupportedResolvedNode {
    fn new(owner: &NodeId, node: &NodeId, reason: impl Into<String>) -> Self {
        Self {
            owner: owner.clone(),
            node: node.clone(),
            reason: reason.into(),
        }
    }
}

/// 0.37.2: reachable dispatch experiment (`MIMI_REACHABLE_DISPATCH=1`).
///
/// Builds a conservative call graph from `CheckedProgram::call_sites()` and
/// returns non-comptime function NodeIds reachable from any `main` entry plus
/// everything referenced from those bodies. This is used only for dispatch
/// statistics / per-function eligible-set filtering under the env flag, not
/// for the ordinary production path.
fn reachable_function_ids(program: &CheckedProgram) -> std::collections::BTreeSet<NodeId> {
    use std::collections::{BTreeSet, HashMap, VecDeque};

    let mut by_qualified: HashMap<String, Vec<NodeId>> = HashMap::new();
    let mut by_bare: HashMap<String, Vec<NodeId>> = HashMap::new();
    for function in program.functions().values() {
        if function.is_comptime {
            continue;
        }
        by_qualified
            .entry(function.qualified_name.clone())
            .or_default()
            .push(function.node_id.clone());
        let bare = function
            .qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(&function.qualified_name)
            .to_string();
        by_bare
            .entry(bare)
            .or_default()
            .push(function.node_id.clone());
    }

    let mut reachable = BTreeSet::new();
    let mut queue = VecDeque::new();
    for function in program.functions().values() {
        if function.is_comptime {
            continue;
        }
        let name = &function.qualified_name;
        let is_entry = name == "main" || name.ends_with("::main") || name.ends_with(":main");
        if is_entry && reachable.insert(function.node_id.clone()) {
            queue.push_back(function.node_id.clone());
        }
    }

    // No entry found: conservative fallback treats every non-comptime function
    // as reachable, so the experimental stats never drop real codegen targets.
    if reachable.is_empty() {
        for function in program.functions().values() {
            if !function.is_comptime {
                reachable.insert(function.node_id.clone());
            }
        }
        return reachable;
    }

    while let Some(owner) = queue.pop_front() {
        let owner_fn = match program.functions().get(&owner) {
            Some(function) => function,
            None => continue,
        };
        let owner_key = format!("function:{}", owner_fn.qualified_name);
        for site in program.call_sites().values() {
            if site.owner != owner_key {
                continue;
            }
            let callee = site.callee.as_str();
            let candidates: Vec<NodeId> = if callee.contains("::") {
                by_qualified.get(callee).cloned().unwrap_or_default()
            } else {
                let mut found = by_qualified.get(callee).cloned().unwrap_or_default();
                if found.is_empty() {
                    found = by_bare.get(callee).cloned().unwrap_or_default();
                }
                found
            };
            for id in candidates {
                if reachable.insert(id.clone()) {
                    queue.push_back(id);
                }
            }
        }
    }
    reachable
}

pub(super) fn require_resolved_native_program(
    program: &CheckedProgram,
) -> Result<(), UnsupportedResolvedNode> {
    let user_flow_count = program
        .flows()
        .values()
        .filter(|flow| matches!(flow.origin, crate::core::Origin::User(_)))
        .count();
    if user_flow_count != 0
        || !program.actors().is_empty()
        || !program.sessions().is_empty()
        || !program.protocols().is_empty()
        || !program.capabilities().is_empty()
        || !program.traits().is_empty()
        || !program.impls().is_empty()
        || !program.extern_blocks().is_empty()
    {
        let owner = program
            .functions()
            .values()
            .next()
            .map(|function| function.node_id.clone())
            .unwrap_or_else(|| NodeId("resolved-native:program".into()));
        return Err(UnsupportedResolvedNode::new(
            &owner,
            &owner,
            format!(
                "declaration kinds beyond plain functions are not in the resolved native slice \
                 (flows={}, actors={}, sessions={}, protocols={}, caps={}, constants={}, traits={}, \
                 impls={}, types={}, externs={})",
                user_flow_count,
                program.actors().len(),
                program.sessions().len(),
                program.protocols().len(),
                program.capabilities().len(),
                program.constants().len(),
                program.traits().len(),
                program.impls().len(),
                program.type_defs().len(),
                program.extern_blocks().len(),
            ),
        ));
    }
    // Constants are allowed, but only materializable (non-Complex) values.
    for constant in program.constants().values() {
        if matches!(constant.value, crate::core::ResolvedConstValue::Complex) {
            return Err(UnsupportedResolvedNode::new(
                &constant.node_id,
                &constant.node_id,
                "constant with non-materializable value is not in the resolved native slice",
            ));
        }
    }
    for function in program.functions().values() {
        if function.is_comptime {
            continue;
        }
        // P1-1 fix: non-User-origin functions (imported from stdlib or
        // generated by the runtime system) must reject the all-or-nothing
        // path. Their bodies may reference runtime symbols the resolved
        // emitter doesn't track, causing SIGSEGV. The per-function dispatch
        // path (eligible_function_ids) handles these correctly by excluding
        // them and falling back to the legacy emitter.
        if !matches!(function.origin, crate::core::Origin::User(_)) {
            return Err(UnsupportedResolvedNode::new(
                &function.node_id,
                &function.node_id,
                "non-user-origin function requires per-function dispatch (not all-or-nothing)",
            ));
        }
        if function.is_async || function.extern_abi.is_some() || !function.generics.is_empty() {
            return Err(UnsupportedResolvedNode::new(
                &function.node_id,
                &function.node_id,
                "async, export, and generic functions are not in the resolved native slice",
            ));
        }
        if function.qualified_name.contains("::") {
            return Err(UnsupportedResolvedNode::new(
                &function.node_id,
                &function.node_id,
                "qualified function symbols are not in the resolved native slice",
            ));
        }
        let callable = program.callable(&function.node_id).ok_or_else(|| {
            UnsupportedResolvedNode::new(
                &function.node_id,
                &function.node_id,
                "missing ResolvedCallable",
            )
        })?;
        require_resolved_native_callable(program, callable)?;
    }
    Ok(())
}

/// Per-function eligibility: returns the set of function NodeIds that can be
/// compiled through the resolved native emitter. Functions that fail the
/// per-function check are simply excluded (not an error).
///
/// Program-level blockers (flows, actors, sessions) still cause a full
/// rejection because they require special compilation infrastructure.
/// Per-function eligibility with structured dispatch statistics (0.34.40).
/// Returns the eligible set plus a stats record covering ALL non-comptime
/// functions (eligible count + skip-reason histogram).
///
/// `verify_contracts` (0.34.41): contract-bearing functions are admitted in
/// both modes — erased at runtime when false (default), guard-emitted by the
/// resolved emitter when true (第二档). The parameter is retained for the
/// stats histogram and future per-mode policy.
pub(super) fn eligible_function_ids_with_stats(
    program: &CheckedProgram,
    verify_contracts: bool,
) -> Result<(std::collections::BTreeSet<NodeId>, DispatchStats), UnsupportedResolvedNode> {
    // 0.32.24: Actors/sessions/protocols/capabilities/externs unblocked at
    // program level. These programs contain regular helper functions (main,
    // utility functions) that don't involve actor/session/protocol compilation.
    // The per-function eligibility checks filter out:
    // - Actor methods (non-User origin, qualified names with "::")
    // - Session/protocol functions (non-User origin)
    // - Functions with actor/session/protocol types in signatures
    //   (require_scalar_type rejects non-scalar Nominal types)
    // The legacy emitter still compiles actor/session/protocol infrastructure
    // (forward declarations, type definitions, method bodies) via compile_actor(),
    // compile_session(), etc. The resolved emitter only compiles the regular
    // functions that pass per-function eligibility.
    //
    // History:
    // - 0.32.8: impls/externs removal caused 2 SIGSEGV (std_strings,
    //   multiple_std_modules) — resolved emitter compiled stdlib function
    //   BODIES. Fixed by per-function source_id filtering (0.32.13).
    // - 0.32.13: impls unblocked (checker auto-generates From/Into impls
    //   for every program). Per-function guards sufficient.
    // - 0.32.20: Flows unblocked. Per-function checks filter transition
    //   functions. Legacy emitter compiles Flow infrastructure.
    // - 0.32.24: Actors/sessions/protocols/caps/externs unblocked.
    //   Same pattern: per-function checks + legacy infrastructure.
    // ⛔ 2026-07-28: traits unblocked. Prelude always loads From/Into traits,
    // so this per-program check was catching EVERY program. The per-function
    // eligibility checks handle function-level filtering. The resolved type
    // lowering now matches legacy ABI for Result/Option. Tuple destructure in
    // the resolved emitter uses alloca+GEP+load instead of extract_value to
    // avoid struct field misordering on cross-function call results.
    // ✅ 0.32.13: impls unblocked. The checker auto-generates From/Into impls
    // for primitive types, making program.impls() non-empty for EVERY program
    // (even without `use std::xxx`). This was silently disabling per-function
    // dispatch for all real programs. The per-function guards (Origin::User,
    // qualified_name "::", non-User-origin callee rejection) are sufficient
    // to prevent stdlib functions from being compiled through resolved.
    // The 0.32.8 SIGSEGV was caused by stdlib module functions being
    // User-origin — the callee origin check (added in 0.32.8) now prevents
    // calls TO those functions from being eligible, which means the calling
    // function is rejected at the per-function level (not program level).
    // Constants must be materializable.
    for constant in program.constants().values() {
        if matches!(constant.value, crate::core::ResolvedConstValue::Complex) {
            return Err(UnsupportedResolvedNode::new(
                &constant.node_id,
                &constant.node_id,
                "constant with non-materializable value",
            ));
        }
    }
    // Per-function eligibility check.
    // 0.32.13: Determine the entry file's source_id so we can exclude
    // functions imported from module files (which are User-origin but
    // defined in a different source file).
    let entry_source = program.entry_span().map(|s| s.source_id);
    let verbose = std::env::var("MIMI_VERBOSE").is_ok();
    let reachable_only = std::env::var("MIMI_REACHABLE_DISPATCH").is_ok();
    let reachable = if reachable_only {
        reachable_function_ids(program)
    } else {
        std::collections::BTreeSet::new()
    };
    let mut stats = DispatchStats {
        // 0.34.40: program identity is the entry source id (u32). The
        // filename is correlated at the caller / script layer; codegen has
        // no SourceRegistry access from CheckedProgram.
        program: entry_source
            .map(|sid| format!("src-{}", sid.raw()))
            .unwrap_or_else(|| "src-0".to_string()),
        ..Default::default()
    };
    let mut eligible = std::collections::BTreeSet::new();
    for function in program.functions().values() {
        if function.is_comptime {
            continue; // comptime functions are folded, not compiled.
        }
        if reachable_only && !reachable.contains(&function.node_id) {
            continue; // experimental reachable-only dispatch stats.
        }
        stats.total_functions += 1;
        let name = &function.qualified_name;
        if function.is_async || function.extern_abi.is_some() {
            stats.record_skip("async/extern");
            if verbose {
                eprintln!("info: resolved skip '{}': comptime/async/extern", name);
            }
            continue;
        }
        if (!function.generics.is_empty() || function.qualified_name.contains("::"))
            && !matches!(function.origin, crate::core::Origin::User(_))
        {
            // Non-user generic functions and qualified non-user symbols stay
            // on the legacy side. User-origin qualified symbols (actor
            // methods, module-local users) may attempt the resolved slice.
            stats.record_skip("generics/qualified");
            if verbose {
                eprintln!("info: resolved skip '{}': generics/qualified", name);
            }
            continue;
        }
        // S13: exclude non-User-origin functions (imported from stdlib or
        // generated by the runtime system). Their bodies may reference runtime
        // symbols the resolved emitter doesn't track, causing SIGSEGV.
        if !matches!(function.origin, crate::core::Origin::User(_)) {
            stats.record_skip("non-user origin");
            if verbose {
                eprintln!("info: resolved skip '{}': non-user origin", name);
            }
            continue;
        }
        // 0.32.13: exclude functions from module files. Module functions
        // are User-origin (parsed from .mimi) but defined in a different
        // source file than the entry point. Their bodies reference runtime
        // symbols and cross-emitter patterns that cause SIGSEGV when
        // compiled through the resolved emitter.
        // 0.34.42 experiment/slice gate: MIMI_RESOLVED_MODULE_BODIES lifts
        // this filter per module:
        //   =1            lift for ALL module files (full experiment)
        //   =<csv list>   lift only when the source disk path contains one
        //                 of the fragments (e.g. "prelude", "prelude,mymath")
        //   unset         keep the filter (production default)
        if let (Some(entry_src), Origin::User(span)) = (entry_source, &function.origin) {
            if span.source_id != entry_src && !module_bodies_lifted(program, span.source_id) {
                stats.record_skip("module file (source_id mismatch)");
                if verbose {
                    eprintln!(
                        "info: resolved skip '{}': module file (source_id mismatch)",
                        name
                    );
                }
                continue;
            }
        }
        let Some(callable) = program.callable(&function.node_id) else {
            stats.record_skip("no ResolvedCallable");
            if verbose {
                eprintln!("info: resolved skip '{}': no ResolvedCallable", name);
            }
            continue;
        };
        match require_resolved_native_callable_with_source(
            program,
            callable,
            entry_source,
            verify_contracts,
        ) {
            Ok(()) => {
                eligible.insert(function.node_id.clone());
                stats.eligible += 1;
            }
            Err(reason) => {
                stats.record_skip(reason.reason.clone());
                if verbose {
                    eprintln!("info: resolved skip '{}': {}", name, reason.reason);
                }
            }
        }
    }
    Ok((eligible, stats))
}

/// 0.34.42: decide whether the source_id module-file filter is lifted for a
/// given source.
///
/// Resolution order:
/// 1. `MIMI_RESOLVED_MODULE_BODIES=1` lifts globally (experiment mode);
/// 2. `MIMI_RESOLVED_MODULE_BODIES=<csv>` lifts only sources whose disk
///    path / canonical URI / registry key contains one fragment (overrides
///    the default allowlist; an explicitly empty value lifts nothing);
/// 3. unset: the BUILT-IN allowlist. 0.34.42: prelude + mymath (A/B corpus
///    proven). 0.35.7 (dx-backlog #19): + strings + collections — the
///    {ptr,i64}→ptr coercion that blocked their method bodies was fixed by
///    routing the whole str_* builtin family through the string emitters
///    (resolved/mod.rs STRING_ABI_BUILTINS), so trait-impl method bodies
///    compile through the resolved slice again.
/// 0.37.0 (Phase A prep): full-corpus `MIMI_RESOLVED_MODULE_BODIES=1`
///    experiment completed with 120/120 successful dispatch builds, no
///    emit_failed and no corpus crash. The module files that were the
///    remaining `module file (source_id mismatch)` burden in the 0.1.6
///    baseline are now lifted by default: datetime, crypto, csv, env, io,
///    template, time. `main` covers dependency package entry modules
///    (e.g. the `mylib` package used by `tests/real_world/projects/consumer`).
fn module_bodies_lifted(program: &CheckedProgram, source_id: crate::span::SourceId) -> bool {
    let spec = match std::env::var("MIMI_RESOLVED_MODULE_BODIES") {
        Ok(explicit) => explicit.trim().to_string(),
        Err(_) => {
            "prelude,mymath,strings,collections,result,array,datetime,crypto,csv,env,errors,fs,io,iter,json,maps,net,random,set,template,testing,text,time,main"
                .to_string()
        }
    };
    if spec.is_empty() {
        return false;
    }
    if spec == "1" {
        return true;
    }
    let record = program.raw_ast().sources.record(source_id);
    let haystacks: Vec<String> = match record {
        Some(record) => {
            let mut out = Vec::new();
            if let Some(path) = &record.disk_path {
                out.push(path.to_string_lossy().into_owned());
            }
            if let Some(uri) = &record.canonical_uri {
                out.push(uri.clone());
            }
            out.push(record.key.as_str().to_string());
            out
        }
        None => Vec::new(),
    };
    // Match on the module FILE NAME, not a bare substring — a fragment like
    // "prelude" must not accidentally lift an entry file named
    // "my_prelude_hack.mimi"... which is itself a path containing the module
    // name, so require the std-path shape `<fragment>.mimi`.
    let wanted: Vec<String> = spec
        .split(',')
        .map(|f| format!("{}.mimi", f.trim()))
        .collect();
    wanted
        .iter()
        .any(|w| haystacks.iter().any(|h| h.ends_with(w)))
}

pub(super) fn require_resolved_native_callable(
    program: &CheckedProgram,
    callable: &ResolvedCallable,
) -> Result<(), UnsupportedResolvedNode> {
    // All-or-nothing path stays conservative: treat as verify_contracts=true so
    // contract-bearing callables never enter the whole-program resolved slice.
    // Per-function dispatch (eligible_function_ids_with_stats) passes the real
    // flag and admits contracts when runtime guards are disabled (erased).
    require_resolved_native_callable_with_source(program, callable, None, true)
}

fn require_resolved_native_callable_with_source(
    program: &CheckedProgram,
    callable: &ResolvedCallable,
    entry_source: Option<crate::span::SourceId>,
    verify_contracts: bool,
) -> Result<(), UnsupportedResolvedNode> {
    // 0.34.41 (AF-4 前置 2①): contracts enter the resolved slice.
    // 第一档: verify_contracts=false (default) admits them via erasure — the
    // Contract arm is a no-op, exactly matching legacy's default erasure.
    // 第二档: verify_contracts=true (--verify-contracts) also admits them —
    // the resolved emitter now emits the same runtime guards legacy does
    // (requires at entry, ensures at every return point, old() entry
    // snapshots; E0808 abort on violation, mod.rs emit_contract_prologue /
    // emit_ensures_checks). Condition expressions are slice-checked by the
    // Contract arm of require_block; an unsupported condition demotes the
    // function to legacy per-function (fail-closed, no silent guard loss).
    let _ = verify_contracts;
    require_scalar_type(program, &callable.owner, &callable.signature.result)?;
    for parameter in &callable.signature.parameters {
        // 0.34.43 (AF-4 前置 2③): non-self view/mutate borrow parameters
        // enter the resolved slice — declare_callable/bind_parameters use
        // the same pointer ABI as legacy declare_func (callee storage IS the
        // caller's storage; true reference semantics, no writeback step),
        // and the Call arm passes such arguments by address. The self
        // receiver exception keeps value ABI (legacy forward declarations
        // match lower_type output there). require_scalar_type below keeps
        // record/List borrows on legacy (scalar-leaf slice discipline).
        require_scalar_type(program, &callable.owner, &parameter.ty)?;
    }
    require_block(
        program,
        &callable.owner,
        &callable.body.root,
        entry_source,
        &callable.body.locals,
    )
}

fn require_scalar_type(
    program: &CheckedProgram,
    owner: &NodeId,
    ty: &ResolvedTypeId,
) -> Result<(), UnsupportedResolvedNode> {
    match program.resolved_types().get(ty) {
        Some(ResolvedType::Primitive(_)) => Ok(()),
        Some(ResolvedType::Tuple(elements)) => {
            for element in elements {
                require_scalar_type(program, owner, element)?;
            }
            Ok(())
        }
        // 0.32.1: Option/Result are already lowerable in types.rs
        // ({i1, T} and {i1, T, E} structs). Accept them in the eligibility
        // gate so the resolved emitter can handle Option/Result-typed
        // parameters, return values, and local bindings.
        Some(ResolvedType::Option(payload)) => require_scalar_type(program, owner, payload),
        Some(ResolvedType::Result { ok, error }) => {
            require_scalar_type(program, owner, ok)?;
            require_scalar_type(program, owner, error)
        }
        // 0.32.14: Newtype is a transparent wrapper — same LLVM repr as inner.
        Some(ResolvedType::Newtype { inner, .. }) => require_scalar_type(program, owner, inner),
        // C3 (audit 2026-08-03): Any / dynamic_value lowers to an opaque i64
        // handle in types.rs, matching the runtime map value box ABI. Accept
        // it in the per-function slice so stdlib Map/Set wrapper calls that
        // flow through DynamicAny stay on the resolved emitter.
        Some(ResolvedType::DynamicAny { .. }) => Ok(()),
        // Generic parameters use the opaque i64 erasure slot in the resolved
        // slice. This allows simple user polymorphic functions (identity,
        // choose, pair, etc.) to be emitted when the body does not inspect
        // the erased value.
        Some(ResolvedType::GenericParameter(_)) => Ok(()),
        // Reference types lower to opaque pointers in types.rs. They are
        // accepted when the target is scalar so borrow bindings/parameters
        // stay in the per-function slice.
        Some(ResolvedType::Reference { target, .. }) => require_scalar_type(program, owner, target),
        // Ownership (shared/weak) annotations are runtime-transparent for
        // shared values: lower to the annotated target and let the resolved
        // emitter handle method calls / upgrades when supported.
        Some(ResolvedType::Ownership { target, .. }) => require_scalar_type(program, owner, target),
        // 0.32.16: Function types (closures) — LLVM repr is {ptr, ptr}.
        Some(ResolvedType::Function {
            parameters, result, ..
        }) => {
            for param in parameters {
                require_scalar_type(program, owner, param)?;
            }
            require_scalar_type(program, owner, result)
        }
        // 0.32.2: Builtin collection types (List/Map/Set) are lowerable
        // in types.rs. Accept them so the resolved emitter can handle
        // collection-typed parameters, return values, and local bindings.
        // 0.32.5: User-defined record types are also accepted.
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) => {
            match item.as_str() {
                // 0.35.23 deep-eval: `builtin:type:Record` is the type-erased
                // map handle (map_new/map_set/from_json results) — same
                // opaque-i64 lowering as Map/Set. Without it, mimi-log's main
                // (count_by_* → Record) fell back to legacy and hit the legacy
                // List<record> for-loop gap.
                // 0.36.7: the Fault crash-context records
                // (SystemTrace/MemoryDump/PanicPayload) lower in types.rs
                // with legacy-matching layouts — accept them so deep trace
                // field access (`.trace.last_state_name`) keeps main in the
                // resolved slice instead of forcing a legacy fallback that
                // loses the qualified flow-fault field types.
                "builtin:type:AtomicI32"
                | "builtin:type:AtomicI64"
                | "builtin:type:AtomicBool"
                | "builtin:type:Channel"
                | "builtin:type:Mutex"
                | "builtin:type:MutexGuard"
                | "builtin:type:List"
                | "builtin:type:Map"
                | "builtin:type:Set"
                | "builtin:type:Record"
                | "builtin:type:SystemTrace"
                | "builtin:type:MemoryDump"
                | "builtin:type:PanicPayload"
                | "builtin:type:PeerFault"
                | "builtin:type:ExecResult"
                | "builtin:type:StatResult" => {
                    for arg in arguments {
                        require_scalar_type(program, owner, arg)?;
                    }
                    Ok(())
                }
                _ => {
                    // User-defined nominal: accept Record and Enum kinds.
                    // Look up the type definition by matching the
                    // NominalTypeId string against type_defs entries.
                    let item_str = item.as_str();
                    // 0.32.20: Flow state types (state:FlowName::StateName)
                    // are record-like types registered by the legacy emitter
                    // as flow::FlowName::StateName. Accept them — lower_type
                    // handles the actual LLVM type lookup.
                    // v0.34.8: delegated to NominalTypeId single source of truth.
                    if item.nominal_is_flow_state() {
                        for arg in arguments {
                            require_scalar_type(program, owner, arg)?;
                        }
                        return Ok(());
                    }
                    let is_record_or_enum = program.type_defs().values().any(|td| {
                        // NominalTypeId is "type:Name"; qualified_name is "Name".
                        let matches_name = item_str
                            .strip_prefix("type:")
                            .is_some_and(|n| td.qualified_name == n)
                            || td.qualified_name == item_str;
                        matches_name
                            && matches!(
                                td.kind,
                                crate::core::resolved::ResolvedTypeKind::Record
                                    // 0.32.12: Enum types accepted. LLVM
                                    // representation is {i32 tag, i64 payload}.
                                    | crate::core::resolved::ResolvedTypeKind::Enum
                            )
                    });
                    if is_record_or_enum {
                        for arg in arguments {
                            require_scalar_type(program, owner, arg)?;
                        }
                        Ok(())
                    } else {
                        // 0.36.32: SessionChan<T> endpoints are opaque i64
                        // handles at the LLVM level (mirroring Map/Set) — the
                        // typed residual surface is compile-time only
                        // (E0414/E0425/E0426), so no declaration catalog
                        // entry is required for the native slice.
                        // 0.36.35: Flow-state nominals ('state:Flow::State')
                        // are similarly admitted — the resolved emitter
                        // lowers them via the legacy type_defs record layout
                        // (resolved/mod.rs lower_type state: fallback).
                        // Actor handles are opaque i64 endpoints at the LLVM
                        // level (mirroring SessionChan). The runtime actor
                        // dispatch remains in the call/expression layer.
                        if item_str.ends_with("SessionChan")
                            || item_str.starts_with("state:")
                            || item_str.starts_with("actor:")
                            || item_str == "builtin:type:Future"
                        {
                            Ok(())
                        } else {
                            Err(UnsupportedResolvedNode::new(
                                owner,
                                owner,
                                format!(
                                    "nominal type '{item_str}' is not a record or enum in the resolved native slice"
                                ),
                            ))
                        }
                    }
                }
            }
        }
        Some(other) => Err(UnsupportedResolvedNode::new(
            owner,
            owner,
            format!("type {other:?} is not in the resolved native slice"),
        )),
        None => Err(UnsupportedResolvedNode::new(
            owner,
            owner,
            format!("missing canonical type '{}'", ty.as_str()),
        )),
    }
}

fn require_block(
    program: &CheckedProgram,
    owner: &NodeId,
    block: &ResolvedBlock,
    entry_source: Option<crate::span::SourceId>,
    locals: &std::collections::BTreeMap<crate::core::ResolvedLocalId, crate::core::ResolvedLocal>,
) -> Result<(), UnsupportedResolvedNode> {
    for statement in &block.statements {
        if !statement.backend_requirements.is_empty() {
            return Err(UnsupportedResolvedNode::new(
                owner,
                &statement.node_id,
                "unmet body backend requirement",
            ));
        }
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(initializer),
            } => {
                require_binding_pattern(owner, pattern)?;
                require_expr(program, owner, initializer, entry_source, locals)?;
            }
            ResolvedStmtKind::Assign {
                target,
                value,
                conversion,
            } => {
                require_root_place(owner, &statement.node_id, target)?;
                require_conversion(owner, &statement.node_id, conversion.kind)?;
                require_expr(program, owner, value, entry_source, locals)?;
            }
            ResolvedStmtKind::Return { value, conversion } => {
                if let Some(value) = value {
                    require_expr(program, owner, value, entry_source, locals)?;
                }
                if let Some(conversion) = conversion {
                    require_conversion(owner, &statement.node_id, conversion.kind)?;
                }
            }
            ResolvedStmtKind::Expr(expression) => {
                require_expr(program, owner, expression, entry_source, locals)?
            }
            ResolvedStmtKind::Bind {
                pattern,
                initializer: None,
            } => {
                require_binding_pattern(owner, pattern)?;
            }
            ResolvedStmtKind::While { condition, body } => {
                require_condition(program, owner, condition, entry_source, locals)?;
                require_block(program, owner, body, entry_source, locals)?;
            }
            ResolvedStmtKind::For {
                pattern,
                iterable,
                body,
            } => {
                require_binding_pattern(owner, pattern)?;
                match &iterable.kind {
                    ResolvedExprKind::Range { start, end } => {
                        require_integer_expr(program, owner, start, entry_source, locals)?;
                        require_integer_expr(program, owner, end, entry_source, locals)?;
                    }
                    // 0.32.14: `range(start, end)` builtin call — same
                    // semantics as Range { start, end }.
                    ResolvedExprKind::Call(call)
                        if matches!(call.callee, ResolvedCallee::Builtin(ref id) if id.as_str() == "range")
                            && call.arguments.len() == 2 =>
                    {
                        require_integer_expr(
                            program,
                            owner,
                            &call.arguments[0].value,
                            entry_source,
                            locals,
                        )?;
                        require_integer_expr(
                            program,
                            owner,
                            &call.arguments[1].value,
                            entry_source,
                            locals,
                        )?;
                    }
                    // 0.32.8–0.32.9: List iteration — `for x in expr` where
                    // expr: List<T>. Accept any expression (Load, Call,
                    // Project, etc.) whose canonical type is List<T> with
                    // scalar element type.
                    _ => {
                        require_expr(program, owner, iterable, entry_source, locals)?;
                        require_list_iterable_type(program, owner, &iterable.ty)?;
                    }
                }
                require_block(program, owner, body, entry_source, locals)?;
            }
            ResolvedStmtKind::Break(value) => {
                if value.is_some() {
                    return Err(UnsupportedResolvedNode::new(
                        owner,
                        &statement.node_id,
                        "break with a value is not in the resolved native slice",
                    ));
                }
            }
            ResolvedStmtKind::Continue => {}
            ResolvedStmtKind::Scope { body, .. } => {
                // H-8 (full-audit-2026-08-05): scope kind (Unsafe/IeeeFloat/
                // Arena/Allocator wrappers) does not change codegen lowering —
                // the emitter emits the inner block identically for every kind
                // and discards statement values. Accept all kinds so tail
                // wrapper blocks stay on the resolved native slice; float
                // semantics inside IeeeFloat are still gated by the recursive
                // require_block below.
                require_block(program, owner, body, entry_source, locals)?;
            }
            ResolvedStmtKind::Loop(body) => {
                require_block(program, owner, body, entry_source, locals)?;
            }
            // K-5 (audit 2026-08-05, closed 2026-08-07): Drop is a codegen
            // no-op ONLY for non-linear places. Legacy emits mimi_cap_drop
            // for capability variables; the resolved emitter has no cap
            // registry, so a Capability-typed place must fall back to legacy
            // (fail-closed) instead of silently leaking the handle.
            // Non-cap drops are pure no-ops on all three backends (VM/legacy/
            // resolved) — verified empirically.
            // M3 (0.35.37): if a Drop place is NOT found in locals (e.g. a
            // parameter slot the resolved lowering did not register), the old
            // `if let Some` silently accepted the drop — for a Capability the
            // handle then leaked with the resolved emitter's no-op Drop. Fail
            // closed: an unregistered drop place is unsupported.
            ResolvedStmtKind::Drop(places) => {
                for place in places {
                    let local = locals.get(&place.base).ok_or_else(|| {
                        UnsupportedResolvedNode::new(
                            owner,
                            &statement.node_id,
                            "drop place is not in the resolved local table",
                        )
                    })?;
                    require_scalar_type(program, owner, &local.ty)?;
                }
            }
            ResolvedStmtKind::Contract { condition, .. } => {
                require_expr(program, owner, condition, entry_source, locals)?;
            }
            ResolvedStmtKind::Math(conditions) => {
                for condition in conditions {
                    require_expr(program, owner, condition, entry_source, locals)?;
                }
            }
            // NestedCallable: declaration marker for nested functions. The nested
            // function is compiled separately (by the resolved emitter or legacy
            // emitter). This statement is a no-op in both backends.
            ResolvedStmtKind::NestedCallable(_) => {}
            other => {
                return Err(UnsupportedResolvedNode::new(
                    owner,
                    &statement.node_id,
                    format!("statement {other:?} is not in the resolved native slice"),
                ))
            }
        }
    }
    if let Some(result) = &block.result {
        require_expr(program, owner, result, entry_source, locals)?;
    }
    Ok(())
}

fn require_binding_pattern(
    owner: &NodeId,
    pattern: &ResolvedPattern,
) -> Result<(), UnsupportedResolvedNode> {
    match &pattern.kind {
        ResolvedPatternKind::Binding { .. } | ResolvedPatternKind::Wildcard => Ok(()),
        ResolvedPatternKind::Constructor { fields, .. } => {
            for (_, sub_pattern) in fields {
                require_binding_pattern(owner, sub_pattern)?;
            }
            Ok(())
        }
        ResolvedPatternKind::Tuple(sub_patterns) => {
            for sub in sub_patterns {
                require_binding_pattern(owner, sub)?;
            }
            Ok(())
        }
        _ => Err(UnsupportedResolvedNode::new(
            owner,
            &pattern.node_id,
            "only value bindings, wildcards, constructor, and tuples are in the resolved native slice",
        )),
    }
}

fn require_root_place(
    _owner: &NodeId,
    _node: &NodeId,
    place: &ResolvedPlace,
) -> Result<(), UnsupportedResolvedNode> {
    for projection in &place.projections {
        match projection {
            crate::core::ir::ResolvedProjection::Tuple { .. } => {}
            // 0.32.2: Index projections for List/Map element access.
            crate::core::ir::ResolvedProjection::Index { .. } => {}
            // 0.32.5: Field projections for record field access.
            crate::core::ir::ResolvedProjection::Field { .. } => {}
            crate::core::ir::ResolvedProjection::Deref { .. } => {}
        }
    }
    Ok(())
}

fn require_conversion(
    owner: &NodeId,
    node: &NodeId,
    conversion: CheckedConversionKind,
) -> Result<(), UnsupportedResolvedNode> {
    if matches!(
        conversion,
        CheckedConversionKind::Identity
            | CheckedConversionKind::NumericWiden
            | CheckedConversionKind::NumericNarrowChecked
            // 0.32.11: Alias/Newtype conversions are identity at the LLVM
            // level (same representation, different type name). Accept them
            // so programs using type aliases and newtypes are eligible.
            | CheckedConversionKind::AliasWrap
            | CheckedConversionKind::AliasUnwrap
            | CheckedConversionKind::NewtypeWrap
            | CheckedConversionKind::NewtypeUnwrap
            // Ownership annotations are runtime-transparent: shared/weak
            // values use the same LLVM representation as their target.
            | CheckedConversionKind::OwnershipWrap
            | CheckedConversionKind::OwnershipDowngrade
            | CheckedConversionKind::OwnershipRead
            // ContainerErase is a purely-typed set/list erasure: the LLVM
            // representation is the same opaque handle, so the resolved
            // emitter already treats it as identity.
            | CheckedConversionKind::ContainerErase
            // DynamicAnyPack: concrete values passed to stdlib Any-typed
            // wrappers. The resolved emitter lowers DynamicAny to i64 and
            // widens narrow ints; map/set runtime boxes already use the same
            // ABI, so this conversion is supported in the native slice.
            | CheckedConversionKind::DynamicAnyPack
    ) {
        Ok(())
    } else {
        Err(UnsupportedResolvedNode::new(
            owner,
            node,
            format!("conversion {conversion:?} is not in the resolved native slice"),
        ))
    }
}

fn is_custom_try_enum(program: &CheckedProgram, id: &crate::core::ResolvedTypeId) -> bool {
    let Some(crate::core::ResolvedType::Nominal { item, .. }) = program.resolved_types().get(id)
    else {
        return false;
    };
    let type_name = item.as_str().strip_prefix("type:").unwrap_or(item.as_str());
    program.type_defs().values().any(|td| {
        (td.qualified_name == type_name || td.qualified_name == item.as_str())
            && matches!(td.kind, crate::core::resolved::ResolvedTypeKind::Enum)
            && td.variants.iter().any(|(name, _)| name == "Ok")
            && td.variants.iter().any(|(name, _)| name == "Err")
    })
}

fn require_expr(
    program: &CheckedProgram,
    owner: &NodeId,
    expression: &ResolvedExpr,
    entry_source: Option<crate::span::SourceId>,
    locals: &std::collections::BTreeMap<crate::core::ResolvedLocalId, crate::core::ResolvedLocal>,
) -> Result<(), UnsupportedResolvedNode> {
    if !expression.backend_requirements.is_empty() {
        // comptime-evaluate is a pure backend requirement; the resolved
        // emitter evaluates the comptime block at runtime (same value, no
        // separate compile-time evaluator), so accept that one requirement.
        let is_supported_comptime = matches!(expression.kind, ResolvedExprKind::Comptime(_))
            && expression.backend_requirements.iter().all(|r| {
                r.requirement_id == "COMPTIME-PURE-001" && r.capability == "comptime.evaluate"
            });
        if !is_supported_comptime {
            return Err(UnsupportedResolvedNode::new(
                owner,
                &expression.node_id,
                "unmet expression backend requirement",
            ));
        }
    }
    require_scalar_type(program, &expression.node_id, &expression.ty)?;
    match &expression.kind {
        ResolvedExprKind::Literal(_) => Ok(()),
        ResolvedExprKind::Constant(_) => Ok(()),
        ResolvedExprKind::Load(place) => require_root_place(owner, &expression.node_id, place),
        ResolvedExprKind::Tuple(elements) => {
            for element in elements {
                require_expr(program, owner, element, entry_source, locals)?;
            }
            Ok(())
        }
        // 0.32.2: List literals.
        ResolvedExprKind::List(elements) => {
            for element in elements {
                require_expr(program, owner, element, entry_source, locals)?;
            }
            Ok(())
        }
        // 0.32.3: Map/Set literals.
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                require_expr(program, owner, key, entry_source, locals)?;
                require_expr(program, owner, value, entry_source, locals)?;
            }
            Ok(())
        }
        ResolvedExprKind::Set(elements) => {
            for element in elements {
                require_expr(program, owner, element, entry_source, locals)?;
            }
            Ok(())
        }
        // 0.32.5: Record construction.
        ResolvedExprKind::Record { fields, .. } => {
            for field in fields {
                require_expr(program, owner, &field.value, entry_source, locals)?;
            }
            Ok(())
        }
        ResolvedExprKind::Project { value, projection } => {
            match projection {
                crate::core::ir::ResolvedValueProjection::Tuple(_) => {}
                // 0.32.2: Index value projections for List element access
                // on rvalues (e.g. get_list()[0]).
                crate::core::ir::ResolvedValueProjection::Index(index_expr) => {
                    require_expr(program, owner, index_expr, entry_source, locals)?;
                }
                // 0.32.5: Field value projections for record rvalue access.
                crate::core::ir::ResolvedValueProjection::Field(_) => {}
                other => {
                    return Err(UnsupportedResolvedNode::new(
                        owner,
                        &expression.node_id,
                        format!("value projection {other:?} is not in the resolved native slice"),
                    ))
                }
            }
            require_expr(program, owner, value, entry_source, locals)
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            require_expr(program, owner, left, entry_source, locals)?;
            require_expr(program, owner, right, entry_source, locals)
        }
        ResolvedExprKind::Unary { op, operand }
            if matches!(
                op,
                crate::core::ir::ResolvedUnaryOp::Negate | crate::core::ir::ResolvedUnaryOp::Not
            ) =>
        {
            require_expr(program, owner, operand, entry_source, locals)
        }
        // 0.37.x: borrow expressions (`&`, `&mut`) and dereference (`*`)
        // enter the resolved slice with the reference pointer ABI.
        ResolvedExprKind::Unary { op, operand }
            if matches!(
                op,
                crate::core::ir::ResolvedUnaryOp::BorrowShared
                    | crate::core::ir::ResolvedUnaryOp::BorrowMutable
                    | crate::core::ir::ResolvedUnaryOp::Dereference
            ) =>
        {
            require_expr(program, owner, operand, entry_source, locals)
        }
        ResolvedExprKind::Cast { value, conversion } => {
            require_conversion(owner, &expression.node_id, conversion.kind)?;
            require_expr(program, owner, value, entry_source, locals)
        }
        // 0.32.16: LocalClosure calls — indirect call through closure struct.
        ResolvedExprKind::Call(call) if matches!(call.callee, ResolvedCallee::LocalClosure(_)) => {
            for argument in &call.arguments {
                require_conversion(owner, &argument.value.node_id, argument.conversion.kind)?;
                require_expr(program, owner, &argument.value, entry_source, locals)?;
            }
            Ok(())
        }
        ResolvedExprKind::Call(call)
            if matches!(
                call.callee,
                ResolvedCallee::Function(_)
                    | ResolvedCallee::Builtin(_)
                    | ResolvedCallee::Constructor(_)
                    | ResolvedCallee::Transition(_)
                    | ResolvedCallee::ProtocolMethod { .. }
                    | ResolvedCallee::ActorMethod { .. }
                    | ResolvedCallee::Extern(_)
            ) =>
        {
            // 0.36.47/0.36.48: METHOD-LEVEL generic trait methods (map<U>/…)
            // are handled by the legacy monomorphization slice. The resolved
            // ProtocolMethod arm looks the impl method up by its un-instantiated
            // symbol and would call the generic signature (unbound type vars)
            // → invalid LLVM IR → SelectionDAG SIGSEGV.
            //
            // 0.36.48: key off the declared method signature instead of
            // call.type_arguments — resolved lowering packs the IMPL-level
            // generic T into type_arguments for every trait method call
            // 0.37.x: method-level generic trait methods are now allowed to
            // attempt the resolved ProtocolMethod path. If the resolved
            // emitter cannot lower one, per-function dispatch falls back to
            // the legacy monomorphization slice automatically.
            // Reject calls to non-User-origin functions (imported from
            // stdlib or generated by the runtime system). Their LLVM
            // symbols may not be declared when the resolved emitter
            // compiles the caller, causing SIGSEGV. Also reject calls
            // to qualified functions (contains "::").
            if let ResolvedCallee::Function(ref callee_owner) = call.callee {
                if let Some(callee_fn) = program.functions().get(callee_owner) {
                    if callee_fn.qualified_name.contains("::") {
                        return Err(UnsupportedResolvedNode::new(
                            owner,
                            &expression.node_id,
                            format!(
                                "call to qualified function '{}' is not in the resolved native slice",
                                callee_fn.qualified_name
                            ),
                        ));
                    }
                    if !matches!(callee_fn.origin, crate::core::Origin::User(_)) {
                        return Err(UnsupportedResolvedNode::new(
                            owner,
                            &expression.node_id,
                            format!(
                                "call to non-user-origin function '{}' is not in the resolved native slice",
                                callee_fn.qualified_name
                            ),
                        ));
                    }
                    // 0.32.18: calls to module functions (different source_id)
                    // are now ALLOWED. The 0.32.13 restriction was overly
                    // conservative — the 0.32.8 SIGSEGV was caused by the
                    // resolved emitter compiling stdlib function BODIES
                    // (which reference runtime symbols it doesn't track),
                    // not by calling them. Module functions are forward-
                    // declared by the legacy emitter (compile.rs step 1)
                    // before the resolved subset is compiled (step 4), so
                    // the LLVM symbol is always available. The resolved
                    // emitter emits a plain `call` to the declared symbol;
                    // the legacy emitter compiles the callee body in step 5.
                    // Per-function source_id filtering (lines 218-222) still
                    // prevents module function BODIES from being compiled
                    // through the resolved emitter.
                }
            }
            for argument in &call.arguments {
                require_conversion(owner, &argument.value.node_id, argument.conversion.kind)?;
                require_expr(program, owner, &argument.value, entry_source, locals)?;
            }
            Ok(())
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            require_condition(program, owner, condition, entry_source, locals)?;
            require_block(program, owner, then_block, entry_source, locals)?;
            require_block(program, owner, else_block, entry_source, locals)
        }
        ResolvedExprKind::Block(block) => {
            require_block(program, owner, block, entry_source, locals)
        }
        ResolvedExprKind::Comptime(block) => {
            require_block(program, owner, block, entry_source, locals)
        }
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let crate::core::ir::ResolvedFStringPart::Interpolation(expr) = part {
                    require_expr(program, owner, expr, entry_source, locals)?;
                }
            }
            Ok(())
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            require_expr(program, owner, scrutinee, entry_source, locals)?;
            for arm in arms {
                require_match_pattern(owner, &arm.pattern)?;
                if let Some(guard) = &arm.guard {
                    require_condition(program, owner, guard, entry_source, locals)?;
                }
                require_expr(program, owner, &arm.body, entry_source, locals)?;
            }
            Ok(())
        }
        ResolvedExprKind::Scope { body, .. } => {
            // H-8: scope kind does not change codegen lowering; accept all
            // wrapper kinds so tail wrapper blocks (unsafe/ieee_float/arena/
            // alloc) keep their implicit value on the resolved native slice.
            require_block(program, owner, body, entry_source, locals)
        }
        // 0.37.x: Spawn/Await are accepted as eager/synchronous futures in
        // the resolved slice (the resolved emitter produces a completed
        // future immediately; awaiting it loads the stored result).
        ResolvedExprKind::Spawn(value) | ResolvedExprKind::Await(value) => {
            require_expr(program, owner, value, entry_source, locals)
        }
        // 0.32.10: Try expression (`?` operator). The inner value must be
        // Result<T, E> or Option<T>. The Try expression itself has type T
        // (the Ok/Some payload), already checked by require_scalar_type at
        // the top of require_expr.
        ResolvedExprKind::Try { value, .. } => {
            require_expr(program, owner, value, entry_source, locals)?;
            // The inner expression's type must be Result, Option, or a
            // two-variant custom enum with Ok/Err names.
            match program.resolved_types().get(&value.ty) {
                Some(ResolvedType::Result { .. } | ResolvedType::Option(_)) => Ok(()),
                Some(ResolvedType::Nominal { .. }) if is_custom_try_enum(program, &value.ty) => {
                    Ok(())
                }
                Some(other) => Err(UnsupportedResolvedNode::new(
                    owner,
                    &expression.node_id,
                    format!("try inner type {other:?} is not Result, Option, or Ok/Err enum"),
                )),
                None => Err(UnsupportedResolvedNode::new(
                    owner,
                    &expression.node_id,
                    "try inner expression has a missing canonical type",
                )),
            }
        }
        // 0.32.16: Lambda expressions — non-capturing and capturing closures.
        // Capturing closures are emitted with a heap-allocated environment
        // (matching the legacy closure ABI) and fall back only if a capture
        // cannot be lowered by the resolved emitter.
        ResolvedExprKind::Lambda(lambda) => {
            for capture in &lambda.captures {
                if !locals.contains_key(capture) {
                    return Err(UnsupportedResolvedNode::new(
                        owner,
                        &expression.node_id,
                        "captured local is not available in the resolved frame",
                    ));
                }
            }
            require_block(program, owner, &lambda.body, entry_source, locals)
        }
        // 0.32.31: Slice expressions (xs[start:end]). Target must be a List.
        // Start/end are optional (default 0/len). Indices must be integers.
        ResolvedExprKind::Slice { target, start, end } => {
            require_expr(program, owner, target, entry_source, locals)?;
            if let Some(start_expr) = start {
                require_integer_expr(program, owner, start_expr, entry_source, locals)?;
            }
            if let Some(end_expr) = end {
                require_integer_expr(program, owner, end_expr, entry_source, locals)?;
            }
            Ok(())
        }
        // 0.32.32: Old expression (contract `old(x)`). In codegen this is
        // identity — the runtime value IS the "old" value since contracts
        // are erased. Only the verifier gives old() distinct semantics.
        ResolvedExprKind::Old(inner) => require_expr(program, owner, inner, entry_source, locals),
        // 0.32.33: Comprehension ([value for pattern in iterable if guard]).
        // Pattern must be a simple binding. Guard (if present) must be bool.
        ResolvedExprKind::Comprehension {
            pattern,
            value,
            iterable,
            guard,
        } => {
            // Only simple binding patterns supported (same as for-in).
            if !matches!(
                &pattern.kind,
                ResolvedPatternKind::Binding {
                    by_reference: None,
                    ..
                }
            ) {
                return Err(UnsupportedResolvedNode::new(
                    owner,
                    &pattern.node_id,
                    "comprehension pattern must be a simple binding",
                ));
            }
            require_expr(program, owner, iterable, entry_source, locals)?;
            if let Some(guard_expr) = guard {
                require_condition(program, owner, guard_expr, entry_source, locals)?;
            }
            require_expr(program, owner, value, entry_source, locals)
        }
        // 0.32.34: OptionalChain (receiver?.field). Receiver must be Option/Result.
        // The field is projected from the payload record. Result is Option<FieldType>.
        ResolvedExprKind::OptionalChain { receiver, .. } => {
            require_expr(program, owner, receiver, entry_source, locals)?;
            // Receiver must be Option or Result.
            match program.resolved_types().get(&receiver.ty) {
                Some(ResolvedType::Option(_) | ResolvedType::Result { .. }) => Ok(()),
                Some(other) => Err(UnsupportedResolvedNode::new(
                    owner,
                    &expression.node_id,
                    format!("optional chain receiver type {other:?} is not Option/Result"),
                )),
                None => Err(UnsupportedResolvedNode::new(
                    owner,
                    &expression.node_id,
                    "optional chain receiver has no resolved type",
                )),
            }
        }
        // 0.32.35: Callable (first-class function value). Only Function callees
        // that are User-origin and non-qualified. The emitter returns a function
        // pointer to the declared LLVM symbol.
        ResolvedExprKind::Callable(callee) => match callee {
            ResolvedCallee::Function(callee_owner) => {
                if let Some(callee_fn) = program.functions().get(callee_owner) {
                    if callee_fn.qualified_name.contains("::") {
                        return Err(UnsupportedResolvedNode::new(
                            owner,
                            &expression.node_id,
                            "callable reference to qualified function not in resolved slice",
                        ));
                    }
                    if !matches!(callee_fn.origin, crate::core::Origin::User(_)) {
                        return Err(UnsupportedResolvedNode::new(
                            owner,
                            &expression.node_id,
                            "callable reference to non-user-origin function not in resolved slice",
                        ));
                    }
                }
                Ok(())
            }
            _ => Err(UnsupportedResolvedNode::new(
                owner,
                &expression.node_id,
                "only Function callees are supported as first-class values in resolved slice",
            )),
        },
        other => Err(UnsupportedResolvedNode::new(
            owner,
            &expression.node_id,
            format!("expression {other:?} is not in the resolved native slice"),
        )),
    }
}

/// Match arm patterns: only literals, wildcards, simple bindings, and
/// constructor patterns (recursively: constructor fields may contain tuples
/// — 0.36.37, `Err((src, e))` — mirroring `require_binding_pattern`).
fn require_match_pattern(
    owner: &NodeId,
    pattern: &ResolvedPattern,
) -> Result<(), UnsupportedResolvedNode> {
    match &pattern.kind {
        ResolvedPatternKind::Wildcard
        | ResolvedPatternKind::Literal(_)
        | ResolvedPatternKind::Binding {
            by_reference: None, ..
        } => Ok(()),
        // 0.32.6: Constructor patterns for Option/Result match arms.
        // Field sub-patterns must also be in the slice.
        ResolvedPatternKind::Constructor { fields, .. } => {
            for (_, sub_pattern) in fields {
                require_match_pattern(owner, sub_pattern)?;
            }
            Ok(())
        }
        ResolvedPatternKind::Tuple(sub_patterns) => {
            for sub_pattern in sub_patterns {
                require_match_pattern(owner, sub_pattern)?;
            }
            Ok(())
        }
        _ => Err(UnsupportedResolvedNode::new(
            owner,
            &pattern.node_id,
            "only literal, wildcard, binding, constructor, and tuple match patterns are in the resolved native slice",
        )),
    }
}

/// Condition expressions must be canonical `bool`.
fn require_condition(
    program: &CheckedProgram,
    owner: &NodeId,
    condition: &ResolvedExpr,
    entry_source: Option<crate::span::SourceId>,
    locals: &std::collections::BTreeMap<crate::core::ResolvedLocalId, crate::core::ResolvedLocal>,
) -> Result<(), UnsupportedResolvedNode> {
    require_expr(program, owner, condition, entry_source, locals)?;
    match program.resolved_types().get(&condition.ty) {
        Some(ResolvedType::Primitive(PrimitiveType::Bool)) => Ok(()),
        Some(other) => Err(UnsupportedResolvedNode::new(
            owner,
            &condition.node_id,
            format!("condition type {other:?} is not bool"),
        )),
        None => Err(UnsupportedResolvedNode::new(
            owner,
            &condition.node_id,
            "condition has a missing canonical type",
        )),
    }
}

/// Range bounds must be signed or unsigned integers (not float/bool).
fn require_integer_expr(
    program: &CheckedProgram,
    owner: &NodeId,
    expression: &ResolvedExpr,
    entry_source: Option<crate::span::SourceId>,
    locals: &std::collections::BTreeMap<crate::core::ResolvedLocalId, crate::core::ResolvedLocal>,
) -> Result<(), UnsupportedResolvedNode> {
    require_expr(program, owner, expression, entry_source, locals)?;
    match program.resolved_types().get(&expression.ty) {
        Some(ResolvedType::Primitive(
            PrimitiveType::I8
            | PrimitiveType::I16
            | PrimitiveType::I32
            | PrimitiveType::I64
            | PrimitiveType::I128
            | PrimitiveType::Isize
            | PrimitiveType::U8
            | PrimitiveType::U16
            | PrimitiveType::U32
            | PrimitiveType::U64
            | PrimitiveType::U128
            | PrimitiveType::Usize,
        )) => Ok(()),
        Some(other) => Err(UnsupportedResolvedNode::new(
            owner,
            &expression.node_id,
            format!("range bound type {other:?} is not an integer"),
        )),
        None => Err(UnsupportedResolvedNode::new(
            owner,
            &expression.node_id,
            "range bound has a missing canonical type",
        )),
    }
}

/// For-in-list iterable type must be `List<T>` where T is a scalar type
/// acceptable by the resolved native slice.
fn require_list_iterable_type(
    program: &CheckedProgram,
    owner: &NodeId,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(), UnsupportedResolvedNode> {
    match program.resolved_types().get(ty) {
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) if item.as_str() == "builtin:type:List" => {
            // List<T>: element type must be scalar.
            for arg in arguments {
                require_scalar_type(program, owner, arg)?;
            }
            Ok(())
        }
        Some(other) => Err(UnsupportedResolvedNode::new(
            owner,
            owner,
            format!("for-in iterable type {other:?} is not List<T>"),
        )),
        None => Err(UnsupportedResolvedNode::new(
            owner,
            owner,
            "for-in iterable has a missing canonical type",
        )),
    }
}
