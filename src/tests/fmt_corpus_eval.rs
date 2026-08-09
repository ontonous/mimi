//! Corpus-level `mimi fmt` evaluation regression locks (0.35.18, dx-backlog #4).
//! Findings: devdocs/v0.35/fmt-eval-0.35.18.md.
//!
//! The existing fmt tests (fmt.rs / fmt_edge_cases.rs / audit_fix_fmt_lint.rs)
//! lock single-file properties (multi-char operator gluing, string/comment
//! preservation, idempotency, token-stream preservation). These tests extend
//! the same properties to the **whole corpus**: every standalone `.mimi`
//! program under `demos/` and `examples/` must be
//!   1. idempotent: fmt(fmt(x)) == fmt(x), and
//!   2. token-stream preserving: the non-structural token sequence (ignoring
//!      Newline/Indent/Dedent/Eof) is identical before and after formatting.
//!
//! Token-stream preservation is the strongest practical semantic-safety
//! proof available at the parser level: identical token sequence (outside
//! whitespace-only changes) implies an identical AST, hence identical
// semantics. The corpus scope is restricted to self-contained programs so
// the test never depends on stdlib directory context or project manifests.

/// Locate the repository root (mirrors `mimi_bin` in audit_fix_fmt_lint.rs).
fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Collect every `.mimi` file under `demos/` and `examples/` (sorted).
/// `test_*` scratch files under demos/ are excluded — they are ephemeral
/// fixtures, not corpus members.
fn corpus_files() -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for sub in ["demos", "examples"] {
        let dir = repo_root().join(sub);
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {}", dir.display(), e))
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().map(|ext| ext == "mimi").unwrap_or(false)
                    && !p
                        .file_stem()
                        .map(|s| s.to_string_lossy().starts_with("test_"))
                        .unwrap_or(false)
            })
            .collect();
        entries.sort();
        files.extend(entries);
    }
    files
}

/// Non-structural token kinds of a source (Newline/Indent/Dedent/Eof dropped).
fn token_kinds(src: &str) -> Vec<String> {
    crate::lexer::Lexer::new(src)
        .tokenize()
        .unwrap_or_else(|e| panic!("corpus source must tokenize: {e}"))
        .iter()
        .filter_map(|t| match t.kind {
            crate::lexer::TokenKind::Newline
            | crate::lexer::TokenKind::Indent
            | crate::lexer::TokenKind::Dedent
            | crate::lexer::TokenKind::Eof => None,
            _ => Some(format!("{:?}", t.kind)),
        })
        .collect()
}

#[test]
fn fmt_corpus_idempotent_and_token_stream_preserved() {
    let files = corpus_files();
    assert!(
        files.len() >= 20,
        "corpus unexpectedly small ({} files) — demos/examples coverage lost",
        files.len()
    );
    let formatter = crate::fmt::Formatter::new();
    let mut failures = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let once = formatter.format(&src);
        let twice = formatter.format(&once);
        if once != twice {
            failures.push(format!(
                "{}: not idempotent (fmt(fmt(x)) != fmt(x))",
                path.display()
            ));
            continue;
        }
        if token_kinds(&src) != token_kinds(&once) {
            failures.push(format!(
                "{}: formatting changed the token stream",
                path.display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "fmt corpus evaluation failed ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn fmt_corpus_std_maps_survives_round_trip() {
    // 0.35.18 evaluation hiccup: std/maps.mimi's `List<(string, Any)>` was
    // suspected of breaking after formatting, but the failure was a
    // directory-context artifact (stdlib `Any` is only visible under std/).
    // Lock the real invariant: the stdlib file formats idempotently and its
    // token stream survives (identical token sequence ⇒ identical AST).
    let path = repo_root().join("std/maps.mimi");
    let src = std::fs::read_to_string(&path).expect("read std/maps.mimi");
    let formatter = crate::fmt::Formatter::new();
    let once = formatter.format(&src);
    assert_eq!(
        formatter.format(&once),
        once,
        "std/maps.mimi not idempotent"
    );
    assert_eq!(
        token_kinds(&src),
        token_kinds(&once),
        "std/maps.mimi token stream changed by fmt"
    );
}
