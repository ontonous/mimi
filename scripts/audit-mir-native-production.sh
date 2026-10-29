#!/usr/bin/env bash
# Canonical MIR native production boundary audit.
#
# The native adapter must fail through NativeMirError before LLVM materializes
# malformed MIR.  Test fixtures may use expect/panic assertions, but the
# production portion of the adapter must not contain those escape hatches or
# direct collection indexing.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TARGET_DIR="$PROJECT_DIR/src/codegen/mir"

printf 'schema=canonical-mir-native-production-audit-v1\n'
printf 'root=%s\n' "$PROJECT_DIR"

TARGET_DIR="$TARGET_DIR" python3 - "$@" <<'PY'
import bisect
import os
import re
import sys
from pathlib import Path

class ScanError(Exception):
    pass


def lex(source):
    """Tokenize enough Rust syntax to scope attributes and ignore trivia/literals."""
    tokens = []
    i = 0
    n = len(source)

    def add(kind, value, start, end, decoded=None):
        tokens.append({"kind": kind, "value": value, "start": start, "end": end, "decoded": decoded})

    while i < n:
        ch = source[i]
        if ch.isspace():
            i += 1
            continue
        if source.startswith("//", i):
            end = source.find("\n", i + 2)
            i = n if end < 0 else end
            continue
        if source.startswith("/*", i):
            start = i
            depth = 1
            i += 2
            while i < n and depth:
                if source.startswith("/*", i):
                    depth += 1
                    i += 2
                elif source.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            if depth:
                raise ScanError(f"unclosed block comment at byte {start}")
            continue

        # Raw string forms: r##"..."##, br##"..."##, and cr##"..."##.
        raw_prefix = None
        for prefix in ("br", "cr", "r"):
            if source.startswith(prefix, i):
                j = i + len(prefix)
                while j < n and source[j] == "#":
                    j += 1
                if j < n and source[j] == '"':
                    raw_prefix = (prefix, j, j - (i + len(prefix)))
                    break
        if raw_prefix:
            prefix, quote, hashes = raw_prefix
            start = i
            terminator = '"' + ('#' * hashes)
            end = source.find(terminator, quote + 1)
            if end < 0:
                raise ScanError(f"unclosed raw string at byte {start}")
            literal = source[start : end + len(terminator)]
            decoded = source[quote + 1 : end]
            add("literal", literal, start, end + len(terminator), decoded)
            i = end + len(terminator)
            continue

        # Ordinary and byte/C string literals. Keep decoded text for #[path].
        string_start = i
        quote_at = None
        if ch == '"':
            quote_at = i
        elif ch in "bc" and i + 1 < n and source[i + 1] == '"':
            quote_at = i + 1
        if quote_at is not None:
            i = quote_at + 1
            escaped = False
            while i < n:
                if escaped:
                    escaped = False
                elif source[i] == "\\":
                    escaped = True
                elif source[i] == '"':
                    i += 1
                    break
                i += 1
            else:
                raise ScanError(f"unclosed string literal at byte {string_start}")
            literal = source[string_start:i]
            raw_body = source[quote_at + 1 : i - 1]
            try:
                decoded = bytes(raw_body, "utf-8").decode("unicode_escape")
            except UnicodeDecodeError:
                decoded = None
            add("literal", literal, string_start, i, decoded)
            continue

        # Character literals are opaque; a lifetime like 'static is a token.
        char_start = i
        if ch == "'" or (ch == "b" and i + 1 < n and source[i + 1] == "'"):
            quote = i if ch == "'" else i + 1
            j = quote + 1
            if j < n and source[j] == "\\":
                j += 1
                if j < n and source[j] == "u" and j + 1 < n and source[j + 1] == "{":
                    close_brace = source.find("}", j + 2)
                    j = n if close_brace < 0 else close_brace + 1
                elif j < n and source[j] == "x":
                    j += 3
                else:
                    j += 1
            else:
                j += 1
            if j < n and source[j] == "'":
                add("literal", source[char_start : j + 1], char_start, j + 1)
                i = j + 1
                continue
            # Rust lifetime or label; it has no delimiter characters.
            add("punct", "'", i, i + 1)
            i += 1
            continue

        if ch.isalpha() or ch == "_":
            start = i
            i += 1
            while i < n and (source[i].isalnum() or source[i] == "_"):
                i += 1
            add("ident", source[start:i], start, i)
            continue

        add("punct", ch, i, i + 1)
        i += 1

    matching = {}
    stack = []
    opening = {"(": ")", "[": "]", "{": "}"}
    closing = {v: k for k, v in opening.items()}
    for index, token in enumerate(tokens):
        value = token["value"]
        if value in opening:
            stack.append((index, value))
        elif value in closing:
            if not stack or stack[-1][1] != closing[value]:
                raise ScanError(f"unbalanced delimiter {value!r} at byte {token['start']}")
            start, _ = stack.pop()
            matching[start] = index
            matching[index] = start
    if stack:
        token = tokens[stack[-1][0]]
        raise ScanError(f"unclosed delimiter {token['value']!r} at byte {token['start']}")
    return tokens, matching


def attr_group(tokens, matching, index):
    if index >= len(tokens) or tokens[index]["value"] != "#":
        return None
    cursor = index + 1
    inner = cursor < len(tokens) and tokens[cursor]["value"] == "!"
    if inner:
        cursor += 1
    if cursor >= len(tokens) or tokens[cursor]["value"] != "[" or cursor not in matching:
        return None
    return inner, cursor, matching[cursor], cursor + 1


def split_args(tokens, start, end, matching):
    parts = []
    part = []
    i = start
    while i < end:
        if tokens[i]["value"] == ",":
            parts.append(part)
            part = []
            i += 1
            continue
        if tokens[i]["value"] in ("(", "[", "{") and i in matching:
            part.extend(range(i, matching[i] + 1))
            i = matching[i] + 1
        else:
            part.append(i)
            i += 1
    if part:
        parts.append(part)
    return parts


def cfg_test_only(tokens, matching, attr):
    """True only when the cfg expression proves this item cannot be production."""
    _, left, right, content = attr
    if content >= right or tokens[content]["value"] != "cfg":
        return False, False
    if content + 1 >= right or tokens[content + 1]["value"] != "(":
        raise ScanError("malformed cfg attribute")
    close = matching.get(content + 1)
    if close != right - 1:
        raise ScanError("malformed cfg attribute group")
    expr = list(range(content + 2, close))
    has_test = any(tokens[k]["value"] == "test" for k in expr)

    def guaranteed_test(ids):
        if len(ids) == 1 and tokens[ids[0]]["value"] == "test":
            return True
        if len(ids) >= 3 and tokens[ids[1]]["value"] == "(":
            group_close = matching.get(ids[1])
            if group_close != ids[-1]:
                return False
            name = tokens[ids[0]]["value"]
            children = split_args(tokens, ids[1] + 1, group_close, matching)
            if name == "all":
                return any(guaranteed_test(child) for child in children)
            if name == "any":
                return bool(children) and all(guaranteed_test(child) for child in children)
            if name == "not" and len(children) == 1:
                child = children[0]
                # not(not(test)) is the only negated form accepted here.
                return (
                    len(child) >= 3
                    and tokens[child[0]]["value"] == "not"
                    and tokens[child[1]]["value"] == "("
                    and matching.get(child[1]) == child[-1]
                    and child[-2] == child[1] + 1
                    and tokens[child[-2]]["value"] == "test"
                )
        return False

    return guaranteed_test(expr), has_test


ITEM_WORDS = {
    "fn", "mod", "impl", "trait", "enum", "struct", "union", "use", "type",
    "static", "const", "extern", "macro", "macro_rules", "let",
}
BODY_ITEMS = {"fn", "mod", "impl", "trait", "enum", "struct", "union", "extern", "macro", "macro_rules"}
MODIFIERS = {"unsafe", "async", "default", "auto"}


def item_kind(tokens, matching, start):
    i = start
    while i < len(tokens):
        value = tokens[i]["value"]
        if value == "pub":
            i += 1
            if i < len(tokens) and tokens[i]["value"] == "(":
                i = matching.get(i, i) + 1
            continue
        if value in MODIFIERS:
            i += 1
            continue
        if value == "const" and i + 1 < len(tokens) and tokens[i + 1]["value"] == "fn":
            i += 1
            continue
        if value == "extern":
            if i + 1 < len(tokens) and tokens[i + 1]["kind"] == "literal":
                if i + 2 < len(tokens) and tokens[i + 2]["value"] == "fn":
                    return "fn", i + 2
            return "extern", i
        if value in ITEM_WORDS:
            return value, i
        if value in ("(", "[", "{") and i in matching:
            i = matching[i] + 1
            continue
        # Attributes can also be attached to local statements. Treat an unknown
        # beginning as a statement and terminate it at its top-level semicolon.
        return "statement", start
    return None, start


def item_end(tokens, matching, start):
    kind, keyword_index = item_kind(tokens, matching, start)
    if kind is None:
        raise ScanError(f"attribute at token {start} is not followed by a Rust item")
    i = keyword_index
    angle_depth = 0
    while i < len(tokens):
        value = tokens[i]["value"]
        if value in ("(", "[", "{") and i in matching:
            if value == "{" and angle_depth == 0 and kind in BODY_ITEMS:
                return matching[i]
            i = matching[i] + 1
            continue
        if value == "<":
            angle_depth += 1
        elif value == ">" and angle_depth > 0:
            # Do not treat the `>` in -> as a generic close.
            if i == 0 or tokens[i - 1]["value"] != "-":
                angle_depth -= 1
        elif value == ";" and angle_depth == 0:
            return i
        i += 1
    raise ScanError(f"could not find the end of attributed {kind} item at token {start}")


def path_attribute(tokens, attributes):
    for attr in attributes:
        _, left, right, content = attr
        if content + 2 < right and tokens[content]["value"] == "path" and tokens[content + 1]["value"] == "=":
            value = tokens[content + 2]
            if value["kind"] != "literal" or value["decoded"] is None:
                raise ScanError("#[path] must use a decodable string literal")
            return value["decoded"]
    return None


def line_at(source, offset):
    return source.count("\n", 0, offset) + 1


def macro_token_ranges(tokens, matching):
    ranges = []
    for index, token in enumerate(tokens):
        if token["value"] in ("(", "[", "{") and index in matching and index > 0 and tokens[index - 1]["value"] == "!":
            ranges.append((index, matching[index]))
    return ranges


def in_ranges(index, ranges):
    return any(start < index < end for start, end in ranges)


def rust_module_dir(path):
    """Return the directory Rust uses for external modules declared in path."""
    path = Path(path)
    if path.name in {"mod.rs", "lib.rs", "main.rs"}:
        return path.parent
    return path.parent / path.stem


def item_attributes_before(tokens, matching, item_index):
    """Read contiguous outer attributes immediately preceding a module item."""
    attributes = []
    cursor = item_index - 1
    while cursor >= 0:
        value = tokens[cursor]["value"]
        if value in MODIFIERS or value == "pub":
            cursor -= 1
            continue
        if value == ")" and cursor in matching:
            opening = matching[cursor]
            if opening > 0 and tokens[opening - 1]["value"] == "pub":
                cursor = opening - 2
                continue
        if value == "]" and cursor in matching:
            opening = matching[cursor]
            if opening > 0 and tokens[opening - 1]["value"] == "#":
                attributes.append(attr_group(tokens, matching, opening - 1))
                cursor = opening - 2
                continue
        break
    return [attribute for attribute in reversed(attributes) if attribute is not None]


def module_references(tokens, matching, path, skipped, macro_ranges, file_test_only=False):
    """Resolve simple external `mod name;` declarations and retain cfg(test) scope."""
    inline_modules = []
    for index, token in enumerate(tokens):
        if token["value"] != "mod" or in_ranges(index, macro_ranges):
            continue
        if index + 2 >= len(tokens) or tokens[index + 1]["kind"] != "ident":
            continue
        if tokens[index + 2]["value"] == "{" and index + 2 in matching:
            inline_modules.append((index, matching[index + 2], tokens[index + 1]["value"]))

    references = []
    for index, token in enumerate(tokens):
        if token["value"] != "mod" or in_ranges(index, macro_ranges):
            continue
        if index + 2 >= len(tokens) or tokens[index + 1]["kind"] != "ident":
            continue
        if tokens[index + 2]["value"] != ";":
            continue

        name = tokens[index + 1]["value"]
        attrs = item_attributes_before(tokens, matching, index)
        explicit_path = path_attribute(tokens, attrs)
        base = rust_module_dir(path)
        for start, end, inline_name in sorted(inline_modules):
            if start < index < end:
                base /= inline_name

        candidates = (
            [Path(path).parent / explicit_path]
            if explicit_path is not None
            else [base / f"{name}.rs", base / name / "mod.rs"]
        )
        existing = [candidate.resolve() for candidate in candidates if candidate.is_file()]
        # If both Rust module layouts exist, leave the files in the audited
        # stream. The source is ambiguous and we must not exclude either one.
        if len(existing) == 1:
            references.append((existing[0], file_test_only or skipped[index]))
    return references


def source_scan(source, path, scan_patterns=True):
    tokens, matching = lex(source)
    macro_ranges = macro_token_ranges(tokens, matching)
    skipped = [False] * len(tokens)
    external_tests = []
    errors = []
    i = 0
    while i < len(tokens):
        parsed = attr_group(tokens, matching, i)
        if parsed is None or in_ranges(i, macro_ranges):
            i += 1
            continue
        inner, left, right, _ = parsed
        attr_start = i
        attrs = []
        cursor = i
        has_test_only = False
        has_ambiguous_test = False
        while cursor < len(tokens):
            current = attr_group(tokens, matching, cursor)
            if current is None or in_ranges(cursor, macro_ranges):
                break
            is_inner, attr_left, attr_right, content = current
            attrs.append(current)
            only_test, has_test = cfg_test_only(tokens, matching, current)
            has_test_only = has_test_only or only_test
            has_ambiguous_test = has_ambiguous_test or (has_test and not only_test)
            cursor = attr_right + 1
            # Inner attributes only apply at the start of a containing module.
            if is_inner:
                if has_test_only:
                    if attr_start == 0:
                        references = module_references(
                            tokens, matching, path, skipped, macro_ranges, file_test_only=True
                        )
                        return [], [], [f"{path}: file-level #![cfg(test)] is test-only"], references, True
                    errors.append(f"{path}:{line_at(source, tokens[attr_start]['start'])}: nested #![cfg(test)] scope is unsupported")
                break

        if not attrs:
            i += 1
            continue
        if not has_test_only and not has_ambiguous_test:
            i = max(cursor, i + 1)
            continue
        if not has_test_only:
            # Leave ambiguous cfg items in the audited stream. If they carry an
            # external path, report the ambiguity instead of silently omitting it.
            try:
                kind, _ = item_kind(tokens, matching, cursor)
                external_path = path_attribute(tokens, attrs)
                if external_path and kind == "mod":
                    errors.append(f"{path}:{line_at(source, tokens[attr_start]['start'])}: ambiguous cfg(test) on #[path] module; refusing to exclude {external_path}")
            except ScanError as exc:
                errors.append(f"{path}:{line_at(source, tokens[attr_start]['start'])}: {exc}")
            i = max(cursor, i + 1)
            continue
        try:
            end = item_end(tokens, matching, cursor)
            kind, _ = item_kind(tokens, matching, cursor)
            external_path = path_attribute(tokens, attrs)
            if external_path:
                if kind != "mod":
                    raise ScanError("#[path] on cfg(test) is not attached to a module")
                if any(attr[0] for attr in attrs):
                    raise ScanError("inner attributes cannot own an external module path")
                if attr_start != 0 and any(
                    tokens[k]["value"] == "{" and matching.get(k, -1) > cursor
                    for k in range(attr_start)
                ):
                    # Relative path rules in nested inline modules are subtle;
                    # do not guess and accidentally blanket-exclude a file.
                    raise ScanError("nested inline #[path] module resolution is unsupported")
                external = (path.parent / external_path).resolve()
                if not external.exists():
                    raise ScanError(f"cfg(test) #[path] target does not exist: {external}")
                external_tests.append(external)
            for k in range(attr_start, end + 1):
                skipped[k] = True
            i = end + 1
        except ScanError as exc:
            errors.append(f"{path}:{line_at(source, tokens[attr_start]['start'])}: {exc}")
            i = max(cursor, i + 1)

    findings = []
    if scan_patterns:
        for index, token in enumerate(tokens):
            if skipped[index]:
                continue
            value = token["value"]
            category = None
            if token["kind"] == "ident" and value in {"unreachable", "panic", "todo"}:
                if (
                    index + 2 < len(tokens)
                    and tokens[index + 1]["value"] == "!"
                    and tokens[index + 2]["value"] in {"(", "[", "{"}
                ):
                    category = "panic_macro"
            if value == "." and index + 2 < len(tokens) and tokens[index + 1]["value"] in {"expect", "unwrap"} and tokens[index + 2]["value"] == "(":
                category = "panic_method"
            if value == "[" and index > 0 and index in matching:
                previous = tokens[index - 1]
                # `name[index]`, `make()[index]`, and `xs[0][index]` all use
                # postfix indexing. The token before `[` must end an expression;
                # type and pattern brackets, array-literal openers, comments,
                # and literal contents do not satisfy that rule.
                inside = tokens[index + 1 : matching[index]]
                is_range = any(
                    inside[k]["value"] == "." and k + 1 < len(inside) and inside[k + 1]["value"] == "."
                    for k in range(len(inside))
                )
                is_pattern_or_literal = previous["value"] in {
                    "let", "in", "if", "while", "for", "return", "yield", "break", "else", "move", "ref",
                }
                is_lifetime_slice_type = (
                    index >= 2
                    and previous["kind"] == "ident"
                    and (
                        tokens[index - 2]["value"] == "'"
                        or (
                            previous["value"] == "mut"
                            and index >= 3
                            and tokens[index - 3]["value"] == "'"
                        )
                    )
                )
                if (
                    (
                        previous["kind"] in {"ident", "literal"}
                        or previous["value"] in {")", "]", "}", "?"}
                    )
                    and not is_pattern_or_literal
                    and not is_range
                    and not is_lifetime_slice_type
                ):
                    category = "direct_index"
            if category:
                text = source.splitlines()[line_at(source, token["start"]) - 1].strip()
                findings.append((category, path, line_at(source, token["start"]), text))
    references = module_references(tokens, matching, path, skipped, macro_ranges)
    return findings, external_tests, errors, references, False


def audit_sources(sources):
    parsed = {}
    module_graph = {}
    errors = []
    excluded = set()
    for path, source in sources.items():
        try:
            findings, external, scan_errors, references, file_test_only = source_scan(source, path)
        except ScanError as exc:
            errors.append(f"{path}: {exc}")
            continue
        resolved_path = path.resolve()
        parsed[resolved_path] = findings
        module_graph[resolved_path] = (references, external, file_test_only)
        errors.extend(scan_errors)
        excluded.update(external)

    # Walk from the shallowest Rust module root so cfg(test) on an external
    # module continues to cover that file's own child modules. Paths that are
    # also reachable from production remain audited.
    roots = [
        path for path in module_graph
        if path.name in {"mod.rs", "lib.rs", "main.rs"}
    ]
    if roots:
        shallowest = min(len(path.parts) for path in roots)
        roots = [path for path in roots if len(path.parts) == shallowest]
    production_reachable = set()
    test_reachable = set()
    visited = set()

    def visit(path, test_context):
        path = path.resolve()
        metadata = module_graph.get(path)
        if metadata is None:
            return
        references, external, file_test_only = metadata
        test_context = test_context or file_test_only
        state = (path, test_context)
        if state in visited:
            return
        visited.add(state)
        (test_reachable if test_context else production_reachable).add(path)
        for child, child_test_only in references:
            visit(child, test_context or child_test_only)
        for child in external:
            visit(child, True)

    for root in roots:
        visit(root, False)
    # Preserve test-only #[path] exclusions even if a containing source was
    # not selected as a crate root, and include their nested module children.
    for path in tuple(excluded):
        visit(path, True)
    excluded.update(test_reachable)
    excluded.difference_update(production_reachable)
    findings = [issue for path, items in parsed.items() if path.resolve() not in excluded for issue in items]
    return findings, errors, excluded


def run_self_tests():
    from tempfile import TemporaryDirectory

    with TemporaryDirectory(prefix="mimi-native-audit-") as temp:
        root = Path(temp)
        test_file = root / "ffi_tests.rs"
        test_file.write_text("fn fixture() { unreachable!(\"test only\"); }\n")
        mod = root / "mod.rs"
        mod_source = '''
#[cfg(test)]
#[path = "ffi_tests.rs"]
mod ffi_tests;

fn production_after() { values[index]; }
'''
        sources = {mod: mod_source, test_file: test_file.read_text()}
        findings, errors, excluded = audit_sources(sources)
        assert not errors, errors
        assert test_file.resolve() in excluded
        assert [(item[0], item[2]) for item in findings] == [("direct_index", 6)], findings

        method_source = '''
impl Adapter {
    #[cfg(test)]
    fn test_helper(&self) { self.items[index].unwrap(); }

    fn production_before(&self) { self.items[index]; }

    #[cfg(test)]
    fn test_after(&self) { panic!("test"); }

    fn production_after(&self) { self.items[index].expect("bad"); }
}
'''
        findings, _, errors, _, _ = source_scan(method_source, root / "methods.rs")
        assert not errors, errors
        assert [item[0] for item in findings] == ["direct_index", "direct_index", "panic_method"], findings

        nested_source = r'''
fn production_before() { values[index]; }
#[cfg(test)]
mod tests {
    const RAW: &str = r###"#[cfg(test)] panic!(\"text\");"###;
    fn test_only() { unreachable!("test"); }
}
fn production_after() { self.args[index]; }
fn mentions_syntax() { let s = "a[0].unwrap() panic!(\"x\")"; }
'''
        findings, _, errors, _, _ = source_scan(nested_source, root / "nested.rs")
        assert not errors, errors
        assert [item[0] for item in findings] == ["direct_index", "direct_index"], findings

        false_positive_source = '''
fn okay(values: &[i32]) -> [i32; 2] {
    fn borrowed<'a>(values: &'a [i32], out: &'a mut [i32]) { let _ = (values, out); }
    let _literal = [1, 2];
    let _raw = r#"self.items[index].unwrap()"#;
    values.len();
    [0, 1]
}
'''
        findings, _, errors, _, _ = source_scan(false_positive_source, root / "false_positives.rs")
        assert not errors, errors
        assert not findings, findings

        expression_index_source = r'''
fn production_calls_and_chains(xs: Vec<Vec<i32>>, index: usize) {
    let _from_call = make_vec() /* trivia */ [index];
    let _nested = xs[0] /* trivia */ [index];
    panic! { "production panic" }
    unreachable! { "production unreachable" }
    todo! /* trivia */ { "production todo" }
}
#[cfg(test)]
fn test_only_calls_and_chains(xs: Vec<Vec<i32>>, index: usize) {
    let _from_call = make_vec()[index];
    let _nested = xs[0][index];
    panic! { "test panic" }
    unreachable! { "test unreachable" }
    todo! { "test todo" }
}
/* make_vec()[index]; xs[0][index]; panic! { } unreachable! { } todo! { } */
fn mentions_only() {
    let _source = "make_vec()[index]; xs[0][index]; panic! { } unreachable! { } todo! { }";
}
'''
        findings, _, errors, _, _ = source_scan(expression_index_source, root / "expression_index.rs")
        assert not errors, errors
        actual = [(item[0], item[2]) for item in findings]
        assert actual == [
            ("direct_index", 3),
            ("direct_index", 4),
            ("direct_index", 4),
            ("panic_macro", 5),
            ("panic_macro", 6),
            ("panic_macro", 7),
        ], findings

        ambiguous_source = '''
#[cfg(any(test, feature = "fixtures"))]
#[path = "ffi_tests.rs"]
mod maybe_fixtures;
'''
        _, external, errors, _, _ = source_scan(ambiguous_source, mod)
        assert errors and "ambiguous cfg(test)" in errors[0], errors
        assert not external

        # A cfg(test) external module carries test scope into its own module
        # tree, while a production sibling with the same unsafe operation is
        # still reported.
        module_root = root / "module_scope"
        module_root.mkdir()
        module_file = module_root / "mod.rs"
        test_child = module_root / "child.rs"
        nested_test_child = module_root / "child" / "nested.rs"
        production_child = module_root / "production.rs"
        nested_test_child.parent.mkdir()
        module_file.write_text(
            "#[cfg(test)]\nmod child;\nmod production;\n", encoding="utf-8"
        )
        test_child.write_text(
            "mod nested;\nfn test_only() { values[index]; }\n", encoding="utf-8"
        )
        nested_test_child.write_text(
            "fn nested_test_only() { values[index]; }\n", encoding="utf-8"
        )
        production_child.write_text(
            "fn production_child() { values[index]; }\n", encoding="utf-8"
        )
        sources = {
            module_file: module_file.read_text(encoding="utf-8"),
            test_child: test_child.read_text(encoding="utf-8"),
            nested_test_child: nested_test_child.read_text(encoding="utf-8"),
            production_child: production_child.read_text(encoding="utf-8"),
        }
        findings, errors, excluded = audit_sources(sources)
        assert not errors, errors
        assert test_child.resolve() in excluded, excluded
        assert nested_test_child.resolve() in excluded, excluded
        assert production_child.resolve() not in excluded, excluded
        assert [(item[1].resolve(), item[0]) for item in findings] == [
            (production_child.resolve(), "direct_index")
        ], findings

    print("native_production_audit_self_test=ok")


if len(sys.argv) > 1:
    if sys.argv[1] != "--self-test" or len(sys.argv) != 2:
        print("usage: audit-mir-native-production.sh [--self-test]", file=sys.stderr)
        sys.exit(2)
    run_self_tests()
    sys.exit(0)

root = Path(os.environ["TARGET_DIR"])
sources = {path.resolve(): path.read_text() for path in sorted(root.rglob("*.rs"))}
findings, errors, excluded = audit_sources(sources)
for error in errors:
    print(f"native_production_scan_error={error}")
for category, path, line_number, line in findings:
    print(f"native_production_issue={category} file={path} line={line_number} text={line}")
print(f"native_production_excluded_test_path_count={len(excluded)}")
print(f"native_production_issue_count={len(findings)}")
if errors:
    print(f"native_production_scan_error_count={len(errors)}")
    sys.exit(2)
if findings:
    sys.exit(1)
print("native_production_audit_status=ok")
PY
