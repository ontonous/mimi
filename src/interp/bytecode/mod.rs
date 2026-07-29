//! Mimi bytecode compilation and register-based VM.
//!
//! Architecture:
//! ```text
//! AST (CheckedProgram) → BytecodeCompiler → BytecodeProgram → BytecodeVM → Value
//! ```
//!
//! The bytecode VM replaces the tree-walking interpreter for eligible functions.
//! It provides 10-30x speedup by eliminating AST match dispatch, HashMap scope
//! lookups, and per-expression recursion overhead.

pub mod compiler;
pub mod instr;
pub mod vm;

pub use compiler::BytecodeCompiler;
pub use instr::{BytecodeProgram, ConstValue, FunctionProto, Op};
pub use vm::BytecodeVM;

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: compile a simple program and run it.
    fn run_program(prog: &BytecodeProgram) -> Result<i64, String> {
        let mut vm = BytecodeVM::new(prog);
        vm.run().map_err(|e| e.to_string())
    }

    #[test]
    fn vm_returns_constant() {
        // func main() -> i32 { 42 }
        let mut main = FunctionProto::new("main".into(), 0);
        let r0 = main.alloc_reg();
        let c42 = main.add_const(ConstValue::Int(42));
        main.emit(Op::LoadConst { rd: r0, idx: c42 });
        main.emit(Op::Ret { ra: r0 });

        let prog = BytecodeProgram {
            functions: vec![main],
            entry: 0,
            builtin_names: vec![],
        };
        assert_eq!(run_program(&prog), Ok(42));
    }

    #[test]
    fn vm_integer_arithmetic() {
        // func main() -> i32 {
        //     let a = 10;
        //     let b = 20;
        //     a + b * 2  // 10 + 40 = 50
        // }
        let mut main = FunctionProto::new("main".into(), 0);
        let r_a = main.alloc_reg(); // 0
        let r_b = main.alloc_reg(); // 1
        let r_tmp = main.alloc_reg(); // 2
        let r_result = main.alloc_reg(); // 3

        let c10 = main.add_const(ConstValue::Int(10));
        let c20 = main.add_const(ConstValue::Int(20));
        let c2 = main.add_const(ConstValue::Int(2));

        main.emit(Op::LoadConst { rd: r_a, idx: c10 });
        main.emit(Op::LoadConst { rd: r_b, idx: c20 });
        main.emit(Op::LoadConst { rd: r_tmp, idx: c2 });
        // r_tmp = b * 2
        main.emit(Op::MulInt { rd: r_tmp, ra: r_b, rb: r_tmp });
        // r_result = a + r_tmp
        main.emit(Op::AddInt { rd: r_result, ra: r_a, rb: r_tmp });
        main.emit(Op::Ret { ra: r_result });

        let prog = BytecodeProgram {
            functions: vec![main],
            entry: 0,
            builtin_names: vec![],
        };
        assert_eq!(run_program(&prog), Ok(50));
    }

    #[test]
    fn vm_function_call() {
        // func add(a: i32, b: i32) -> i32 { a + b }
        // func main() -> i32 { add(3, 4) }
        let mut add_fn = FunctionProto::new("add".into(), 2);
        // params are r0, r1
        let r_sum = add_fn.alloc_reg(); // 2
        add_fn.emit(Op::AddInt { rd: r_sum, ra: 0, rb: 1 });
        add_fn.emit(Op::Ret { ra: r_sum });

        let mut main = FunctionProto::new("main".into(), 0);
        let r_arg0 = main.alloc_reg(); // 0
        let r_arg1 = main.alloc_reg(); // 1
        let r_result = main.alloc_reg(); // 2

        let c3 = main.add_const(ConstValue::Int(3));
        let c4 = main.add_const(ConstValue::Int(4));

        main.emit(Op::LoadConst { rd: r_arg0, idx: c3 });
        main.emit(Op::LoadConst { rd: r_arg1, idx: c4 });
        main.emit(Op::Call {
            rd: r_result,
            func: 0, // add_fn
            args_base: r_arg0,
            argc: 2,
        });
        main.emit(Op::Ret { ra: r_result });

        let prog = BytecodeProgram {
            functions: vec![add_fn, main],
            entry: 1,
            builtin_names: vec![],
        };
        assert_eq!(run_program(&prog), Ok(7));
    }

    #[test]
    fn vm_recursive_fib() {
        // func fib(n: i32) -> i32 {
        //     if n <= 1 { n } else { fib(n-1) + fib(n-2) }
        // }
        // func main() -> i32 { fib(10) }
        let mut fib = FunctionProto::new("fib".into(), 1);
        // r0 = n (param)
        let r_one = fib.alloc_reg(); // 1
        let r_cmp = fib.alloc_reg(); // 2
        let r_n1 = fib.alloc_reg(); // 3
        let r_n2 = fib.alloc_reg(); // 4
        let r_arg = fib.alloc_reg(); // 5
        let r_f1 = fib.alloc_reg(); // 6
        let r_f2 = fib.alloc_reg(); // 7
        let r_sum = fib.alloc_reg(); // 8

        let c1 = fib.add_const(ConstValue::Int(1));
        let c2 = fib.add_const(ConstValue::Int(2));

        // r_one = 1
        fib.emit(Op::LoadConst { rd: r_one, idx: c1 }); // 0
        // r_cmp = (n <= 1)
        fib.emit(Op::LeInt { rd: r_cmp, ra: 0, rb: r_one }); // 1
        // if !r_cmp goto else (instruction 7)
        let jmp_else = fib.emit(Op::JmpIfNot { offset: 0, ra: r_cmp }); // 2
        // then: return n
        fib.emit(Op::Ret { ra: 0 }); // 3
        // else:
        // r_n1 = n - 1
        fib.emit(Op::SubInt { rd: r_n1, ra: 0, rb: r_one }); // 4
        // r_arg = r_n1
        fib.emit(Op::Mov { rd: r_arg, rs: r_n1 }); // 5
        // r_f1 = fib(n-1)
        fib.emit(Op::Call { rd: r_f1, func: 0, args_base: r_arg, argc: 1 }); // 6
        // r_n2 = n - 2
        fib.emit(Op::LoadConst { rd: r_one, idx: c2 }); // 7: reuse r_one for 2
        fib.emit(Op::SubInt { rd: r_n2, ra: 0, rb: r_one }); // 8
        // r_arg = r_n2
        fib.emit(Op::Mov { rd: r_arg, rs: r_n2 }); // 9
        // r_f2 = fib(n-2)
        fib.emit(Op::Call { rd: r_f2, func: 0, args_base: r_arg, argc: 1 }); // 10
        // r_sum = r_f1 + r_f2
        fib.emit(Op::AddInt { rd: r_sum, ra: r_f1, rb: r_f2 }); // 11
        fib.emit(Op::Ret { ra: r_sum }); // 12

        // Patch jump: from instruction 2, jump to instruction 4 (else branch)
        fib.patch_jump_to(jmp_else, 4);

        let mut main = FunctionProto::new("main".into(), 0);
        let r_arg = main.alloc_reg(); // 0
        let r_result = main.alloc_reg(); // 1
        let c10 = main.add_const(ConstValue::Int(10));
        main.emit(Op::LoadConst { rd: r_arg, idx: c10 });
        main.emit(Op::Call { rd: r_result, func: 0, args_base: r_arg, argc: 1 });
        main.emit(Op::Ret { ra: r_result });

        let prog = BytecodeProgram {
            functions: vec![fib, main],
            entry: 1,
            builtin_names: vec![],
        };
        // fib(10) = 55
        assert_eq!(run_program(&prog), Ok(55));
    }

    #[test]
    fn vm_while_loop() {
        // func main() -> i32 {
        //     let mut sum = 0;
        //     let mut i = 0;
        //     while i < 100 {
        //         sum = sum + i;
        //         i = i + 1;
        //     }
        //     sum
        // }
        let mut main = FunctionProto::new("main".into(), 0);
        let r_sum = main.alloc_reg(); // 0
        let r_i = main.alloc_reg(); // 1
        let r_cmp = main.alloc_reg(); // 2
        let r_one = main.alloc_reg(); // 3
        let r_tmp = main.alloc_reg(); // 4

        let c0 = main.add_const(ConstValue::Int(0));
        let c100 = main.add_const(ConstValue::Int(100));
        let c1 = main.add_const(ConstValue::Int(1));

        // sum = 0, i = 0
        main.emit(Op::LoadConst { rd: r_sum, idx: c0 }); // 0
        main.emit(Op::LoadConst { rd: r_i, idx: c0 }); // 1
        main.emit(Op::LoadConst { rd: r_one, idx: c1 }); // 2
        main.emit(Op::LoadConst { rd: r_tmp, idx: c100 }); // 3

        // loop_start:
        // r_cmp = (i < 100)
        let loop_start = main.emit(Op::LtInt { rd: r_cmp, ra: r_i, rb: r_tmp }); // 4
        // if !r_cmp goto end
        let jmp_end = main.emit(Op::JmpIfNot { offset: 0, ra: r_cmp }); // 5
        // sum = sum + i
        main.emit(Op::AddInt { rd: r_sum, ra: r_sum, rb: r_i }); // 6
        // i = i + 1
        main.emit(Op::AddInt { rd: r_i, ra: r_i, rb: r_one }); // 7
        // goto loop_start
        let jmp_loop = main.emit(Op::Jmp { offset: 0 }); // 8
        // end:
        main.emit(Op::Ret { ra: r_sum }); // 9

        // Patch jumps
        // jmp_end: from 5, jump to 9 (Ret)
        main.patch_jump_to(jmp_end, 9);
        // jmp_loop: from 8, jump to 4 (loop_start)
        main.patch_jump_to(jmp_loop, loop_start);

        let prog = BytecodeProgram {
            functions: vec![main],
            entry: 0,
            builtin_names: vec![],
        };
        // sum(0..99) = 4950
        assert_eq!(run_program(&prog), Ok(4950));
    }

    // ═══════════════════════════════════════════════════════════
    // End-to-end: source → parse → compile → VM → result
    // ═══════════════════════════════════════════════════════════

    /// Parse Mimi source, compile to bytecode, run, return exit code.
    fn e2e(src: &str) -> Result<i64, String> {
        let tokens = crate::lexer::Lexer::new(src)
            .tokenize()
            .map_err(|e| format!("lexer: {}", e))?;
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .map_err(|e| format!("parser: {}", e))?;
        let mut compiler = BytecodeCompiler::new();
        let prog = compiler
            .compile_file(&file)
            .map_err(|e| format!("compiler: {}", e))?;
        run_program(&prog)
    }

    #[test]
    fn e2e_constant_return() {
        assert_eq!(e2e("func main() -> i32 { 42 }"), Ok(42));
    }

    #[test]
    fn e2e_arithmetic() {
        assert_eq!(
            e2e("func main() -> i32 { let a = 10; let b = 20; a + b }"),
            Ok(30)
        );
    }

    #[test]
    fn e2e_function_call() {
        assert_eq!(
            e2e("func add(a: i32, b: i32) -> i32 { a + b }
                 func main() -> i32 { add(3, 4) }"),
            Ok(7)
        );
    }

    #[test]
    fn e2e_recursive_fib() {
        assert_eq!(
            e2e("func fib(n: i32) -> i32 {
                     if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
                 }
                 func main() -> i32 { fib(10) }"),
            Ok(55)
        );
    }

    #[test]
    fn e2e_while_loop() {
        assert_eq!(
            e2e("func main() -> i32 {
                     let mut sum = 0
                     let mut i = 0
                     while i < 100 {
                         sum = sum + i
                         i = i + 1
                     }
                     sum
                 }"),
            Ok(4950)
        );
    }

    #[test]
    fn e2e_nested_while() {
        assert_eq!(
            e2e("func main() -> i32 {
                     let mut total = 0
                     let mut y = 0
                     while y < 10 {
                         let mut x = 0
                         while x < 10 {
                             total = total + 1
                             x = x + 1
                         }
                         y = y + 1
                     }
                     total
                 }"),
            Ok(100)
        );
    }

    #[test]
    fn e2e_if_else() {
        assert_eq!(
            e2e("func classify(x: i32) -> i32 {
                     if x > 0 { 1 } else { 0 }
                 }
                 func main() -> i32 { classify(5) + classify(0) }"),
            Ok(1)
        );
    }

    #[test]
    fn e2e_mandelbrot_inner() {
        // Simplified mandelbrot: test float arithmetic + nested loops + function calls.
        let result = e2e("func iterations(cx: f64, cy: f64) -> i32 {
                     let mut zx = 0.0
                     let mut zy = 0.0
                     let mut i = 0
                     while i < 100 {
                         let zx2 = zx * zx
                         let zy2 = zy * zy
                         if zx2 + zy2 > 4.0 { return i }
                         zy = 2.0 * zx * zy + cy
                         zx = zx2 - zy2 + cx
                         i = i + 1
                     }
                     i
                 }
                 func main() -> i32 {
                     let mut total = 0
                     let mut y = 0
                     while y < 10 {
                         let cy = y as f64 / 5.0 - 1.0
                         let mut x = 0
                         while x < 10 {
                             let cx = x as f64 / 5.0 - 1.5
                             total = total + iterations(cx, cy)
                             x = x + 1
                         }
                         y = y + 1
                     }
                     total
                 }");
        assert!(result.is_ok(), "mandelbrot should run: {:?}", result);
    }

    #[test]
    fn e2e_println() {
        let tokens = crate::lexer::Lexer::new(
            "func main() -> i32 { println(42); 0 }",
        )
        .tokenize()
        .unwrap();
        let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
        let mut compiler = BytecodeCompiler::new();
        let prog = compiler.compile_file(&file).unwrap();
        let mut vm = BytecodeVM::new(&prog);
        let code = vm.run().unwrap();
        assert_eq!(code, 0);
        assert_eq!(vm.stdout().trim(), "42");
    }

    #[test]
    fn e2e_fib_with_print() {
        let tokens = crate::lexer::Lexer::new(
            "func fib(n: i32) -> i32 {
                 if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
             }
             func main() -> i32 {
                 println(fib(15))
                 0
             }",
        )
        .tokenize()
        .unwrap();
        let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
        let mut compiler = BytecodeCompiler::new();
        let prog = compiler.compile_file(&file).unwrap();
        let mut vm = BytecodeVM::new(&prog);
        let code = vm.run().unwrap();
        assert_eq!(code, 0);
        assert_eq!(vm.stdout().trim(), "610"); // fib(15) = 610
    }
}
