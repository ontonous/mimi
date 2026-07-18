# QuotedAst ABI v1

Quoted AST has two representations with one deliberately narrow compiled contract.

## Interpreter representation

`interp::QuotedAst` is the semantic representation. `ast_eval` accepts only
`Value::QuoteAst` and returns the evaluated Mimi value or an `InterpError`.
Lambda nodes retain parameters, return metadata, their source body, and a
snapshot of referenced lexical captures. Cast and WhileLet are preserved and
evaluated as nodes. Match is rejected while quoting with the stable error
`quoted AST node 'Match' is unsupported by ABI v1`.

## Runtime representation

`MimiQuotedAst` is the C-compatible tagged-node layout. ABI consumers must query
`mimi_quote_abi_version()` and require version `1`. `QuotedAstTag` discriminants
are append-only for the lifetime of ABI v1; unknown tags are rejected by all
constructors.

Nodes returned by `mimi_quote_new_leaf`, `mimi_quote_new_node`, and
`mimi_quote_new_list` are owned handles. A node/list constructor takes ownership
of every non-null child only when construction succeeds. `mimi_quote_drop`
recursively releases the tree and is idempotent for null, foreign, and already
dropped handles. Accessors validate handles against the live-node registry;
same-layout foreign pointers and stale pointers are rejected.

## Compiled evaluator boundary

ABI v1 does not expose a runtime evaluator for `MimiQuotedAst`. LLVM lowering
accepts `ast_eval` only when quote evaluation completed during compilation; in
that case the argument is already the evaluator result. Runtime-dependent quote
expressions are rejected with
`runtime-dependent quote is unsupported by QuotedAst ABI v1; ast_eval is compile-time only`.
This prevents an AST pointer from being mistaken for a semantically evaluated
value. A future runtime evaluator requires a new ABI version or an additive,
explicit result/error API.
