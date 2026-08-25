//! Builtin registration matrix invariant (0.39.136).
//!
//! Root cause of the seven-unusable-builtins defect: builtins live across
//! FIVE hand-maintained surfaces (canonical arity table, purity list,
//! codegen dispatch, VM registry, checker dispatch arms + optional stdlib
//! wrappers). These tests pin the matrix so drift fails CI instead of a
//! user. Each uncovered name must carry a reason in DELIBERATE_UNCLASSIFIED.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(manifest().join(rel)).expect(rel)
}

/// Precise checker-dispatch names: `"a" | "b" =>` patterns inside the
/// builtin-dispatch block of infer/call/simple.rs.
fn checker_dispatch_names() -> BTreeSet<String> {
    let src = read("src/core/infer/call/simple.rs");
    let i = src.find("'builtin_dispatch:").expect("dispatch marker");
    let mut out = BTreeSet::new();
    let seg = &src[i..];
    // Walk each `=>` and collect the quoted-name chain before it (arms span
    // multiple lines: `"a"\n | "b"\n | "c" => {`).
    let bytes = seg.as_bytes();
    let mut k = 0usize;
    while let Some(arrow) = seg[k..].find("=>") {
        let abs_arrow = k + arrow;
        // Walk backwards over `"name"` and `|` separators.
        let mut j = abs_arrow;
        loop {
            let head = seg[..j].trim_end();
            if !head.ends_with('"') {
                break;
            }
            let close = head.rfind('"').unwrap();
            let open = match head[..close].rfind('"') {
                Some(o) => o,
                None => break,
            };
            let cand = &head[open + 1..close];
            if !cand.is_empty()
                && cand
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                out.insert(cand.to_string());
            }
            // Step past the opening quote; a trailing `|` separator means
            // more names precede on possibly another line.
            j = open;
            let tail = seg[..j].trim_end();
            if tail.ends_with('|') {
                j = tail.len() - 1;
            } else {
                break;
            }
        }
        k = abs_arrow + 2;
    }
    out
}

/// Names exported by any std/*.mimi module (`pub func NAME<..>(`).
fn stdlib_wrappers() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for entry in std::fs::read_dir(manifest().join("std")).expect("std dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("mimi") {
            continue;
        }
        for line in std::fs::read_to_string(&path).expect("read").lines() {
            let rest = match line.strip_prefix("pub func ") {
                Some(r) => r,
                None => continue,
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                out.insert(name);
            }
        }
    }
    out
}

fn classification_names() -> BTreeSet<String> {
    let src = read("src/core/builtins.rs");
    let i = src
        .find("pub fn is_builtin_callable")
        .expect("classification fn");
    let seg = &src[i..];
    seg.lines()
        .filter_map(|l| l.trim().strip_prefix("| "))
        .map(|t| t.trim().trim_matches('"').to_string())
        .collect()
}

/// Codegen dispatch corpus (all files under src/codegen/builtins).
fn codegen_corpus() -> String {
    let dir = manifest().join("src/codegen/builtins");
    let mut corpus = String::new();
    for entry in std::fs::read_dir(&dir).expect("dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            corpus.push_str(&std::fs::read_to_string(path).expect("read"));
        }
    }
    corpus
}

/// VM-internal / meta intrinsics deliberately outside the stable user
/// surface. Every entry needs a reason.
const DELIBERATE_UNCLASSIFIED: &[(&str, &str)] = &[
    ("alloc", "VM arena experiment surface"),
    ("allocator_arena", "VM memory intrinsics"),
    ("allocator_bump", "VM memory intrinsics"),
    ("allocator_system", "VM memory intrinsics"),
    ("arena_reset", "VM memory intrinsics"),
    ("bump_used", "VM memory intrinsics"),
    ("arena_alloc", "VM memory intrinsics"),
    ("ast_dump", "compiler-debug meta tool"),
    ("__slice", "VM compiler intrinsic op"),
    ("deref", "intrinsic; user syntax is `*x`"),
    ("eq", "intrinsic; user syntax is `==`"),
    ("inner", "intrinsic (Result/Option unwrap)"),
    ("fields", "reflection meta surface, unresolved"),
    ("type_fields", "reflection meta surface, unresolved"),
    ("type_variants", "reflection meta surface, unresolved"),
    ("type_name", "reflection meta surface, unresolved"),
    ("clone", "method/copy semantics cover List"),
    ("str", "VM string ctor alias"),
    (
        "filter",
        "bare alias; canonical surface is collections::filter_list",
    ),
    (
        "read_lines_each",
        "callback-streaming IO intrinsic; canonical surface is fs::read_lines",
    ),
    (
        "reduce",
        "bare alias; canonical surface is collections::reduce_list",
    ),
    ("from_json_typed", "experimental typed JSON parse"),
    ("println", "special-cased before builtin resolution"),
    ("unsafe_cast_protocol", "special-cased at call site"),
    ("close", "session statement form owns this spelling"),
];

#[test]
fn every_vm_builtin_is_usable_from_the_checker() {
    let registry = mimi::interp::bytecode::registry::create_registry();
    assert!(registry.len() > 200, "registry unexpectedly small");
    let armed = checker_dispatch_names();
    let wrappers = stdlib_wrappers();

    let unusable: Vec<String> = registry
        .names()
        .into_iter()
        .filter(|n| {
            !armed.contains(n)
                && !wrappers.contains(n)
                && !DELIBERATE_UNCLASSIFIED.iter().any(|(d, _)| d == n)
        })
        .collect();
    assert!(
        unusable.is_empty(),
        "VM-registered builtins unknown to BOTH the checker dispatch and \
         every stdlib module (callers get E0401): {unusable:?}\n\
         Fix: add a dispatch arm in infer/call/simple.rs (+ classification \
         entry in core/builtins.rs) or a documented stdlib wrapper."
    );
}

#[test]
fn every_checker_arm_is_classified_or_wrapped() {
    // int/float lesson: a dispatch arm WITHOUT an is_builtin_callable entry
    // passes check() then crashes resolved lowering (closed Unknown target).
    let arms = checker_dispatch_names();
    let classified = classification_names();
    let wrappers = stdlib_wrappers();

    let gaps: Vec<String> = arms
        .iter()
        .filter(|n| !classified.contains(*n) && !wrappers.contains(*n))
        .filter(|n| !DELIBERATE_UNCLASSIFIED.iter().any(|(d, _)| d == n))
        .cloned()
        .collect();
    assert!(
        gaps.is_empty(),
        "checker arms absent from classification and wrappers — these pass \
         check() then crash resolved lowering: {gaps:?}"
    );
}

#[test]
fn every_vm_builtin_exists_in_codegen_dispatch() {
    let registry = mimi::interp::bytecode::registry::create_registry();
    let corpus = codegen_corpus();
    let wrappers = stdlib_wrappers();
    // expr.rs hosts a secondary native dispatch table for string ops.
    let expr_src = read("src/codegen/expr.rs");

    let missing: Vec<String> = registry
        .names()
        .into_iter()
        .filter(|n| {
            !corpus.contains(&format!("\"{n}\""))
                && !wrappers.contains(n)
                && !expr_src.contains(&format!("\"{n}\""))
                && !DELIBERATE_UNCLASSIFIED.iter().any(|(d, _)| d == n)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "VM-registered builtins absent from every native dispatch site \
         (unsupported-builtin at runtime): {missing:?}"
    );
}

#[test]
fn every_vm_builtin_is_in_the_canonical_tables() {
    let registry = mimi::interp::bytecode::registry::create_registry();
    let missing: Vec<String> = registry
        .names()
        .into_iter()
        .filter(|n| mimi::core::builtins::builtin_arity(n).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "VM-registered builtins missing from canonical builtin_arity: {missing:?}"
    );
}
