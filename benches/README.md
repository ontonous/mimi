# Performance Benchmarks

## Usage

```bash
# Run all benchmarks
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo bench

# Run specific benchmark group
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo bench --bench parser
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo bench --bench codegen
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo bench --bench interp

# HTML report available at target/criterion/report/index.html
```

## Benchmark Groups

| Group | File | What it measures |
|-------|------|-----------------|
| `parser/simple` | parser.rs | Lex + parse a trivial program |
| `parser/complex` | parser.rs | Lex + parse enum/match ADT |
| `parser/500_functions` | parser.rs | Lex + parse 500 functions |
| `parser/deep_nesting_100` | parser.rs | Lex + parse 100-level `if` nesting |
| `codegen/simple` | codegen.rs | Full compile pipeline to LLVM IR |
| `codegen/complex` | codegen.rs | Compile enum/match + arithmetic |
| `codegen/recursive_fib` | codegen.rs | Compile recursive fib(20) |
| `codegen/with_contracts` | codegen.rs | Compile function with requires/ensures |
| `codegen/large_enum_20` | codegen.rs | Compile enum with 20 variants |
| `interp/simple` | interp.rs | Parse + type-check + run trivial |
| `interp/fib_30` | interp.rs | Interpret recursive fib(30) |
| `interp/prime_check` | interp.rs | Interpret prime sieve |
| `interp/list_sum_10` | interp.rs | Interpret list sum of 10 items |
| `interp/list_sum_100` | interp.rs | Interpret list sum of 100 items |
| `interp/match_enum` | interp.rs | Interpret enum pattern matching |
| `interp/contract_check` | interp.rs | Interpret with runtime contract verification |

## Baseline Tracking

Run `cargo bench` before and after changes to compare.
The HTML report shows regression/Z-score analysis automatically.
