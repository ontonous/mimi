//! 0.35.3 L1 — SD-9 链式末端检查收敛（Float Chain Convergence）。
//!
//! IEEE 754 NaN/Inf 传播性（devdocs/v0.35/trap-decomposition-0.35.2.md §3）：
//! 纯 f64/f32 代数链中，中间结果一旦非有限，链末端必然非有限（NaN ⊛ x = NaN；
//! Inf 参与的 +/−/×/÷ 结果 ∈ {NaN, ±Inf}）。因此"链中每个 op 检查 finiteness"
//! 可等效收敛为"链末端检查"（逆否：末端有限 ⇒ 整条链有限），语义保持。
//!
//! 本 pass 在 O1 优化管线前对每个函数做 IR 变换：
//!   1. 收集所有 SD-9 检查点（特征指令：`fcmp uno x, x` 的 is_nan）；
//!   2. 被检查值 `x` 的所有用户（排除检查部件自身）若全部是"受检 f64/f32 代数
//!      op"（其自身结果也在检查点集合中）→ `x` 是链中继点，删除其检查；
//!   3. 否则 `x` 是链末端/观察点（比较/存储/调用参数/返回/phi 消费/不受检的
//!      ieee 块内 op），保留检查。
//!
//! 边界纪律（分级表 0.35.2 §4）：
//!   - 仅处理非 fallible 路径（trap 块含 `mimi_trap_float_not_finite` 调用）；
//!     fallible multi-target 的 Fault 吸收点是语义的一部分，不收敛；
//!   - 不处理 ieee_float 块内 op（其结果不在检查点集合，天然被排除）；
//!   - 中继点检查删除后，非有限传播到末端检查捕获——两次 trap 之间只有纯
//!     算术指令（无副作用），可观测行为等价（E0813 abort）。
//!
//! 仅 O1 路径调用（O0 保持逐点检查，行为完全不变）。

use inkwell::llvm_sys::core::*;
use inkwell::llvm_sys::prelude::*;
use inkwell::llvm_sys::{LLVMRealPredicate, LLVMTypeKind, LLVMOpcode};
use std::collections::HashSet;

/// 检查点：`x` 被 `check_float_finite`/`enforce_float_finite` 检查。
struct CheckPoint {
    x: LLVMValueRef,
    /// is_nan = `fcmp uno x, x`
    is_nan: LLVMValueRef,
    /// fabs(x) 调用（is_inf 的输入），用于排除检查部件
    fabs_call: Option<LLVMValueRef>,
    /// 条件分支 `br not_finite trap_bb ok_bb`
    cond_br: LLVMValueRef,
    /// 分支后继续执行的块（非 trap 侧）
    ok_bb: LLVMBasicBlockRef,
}

/// 对模块中所有函数执行 SD-9 链式末端检查收敛。
///
/// 返回值：是否发生了任何重写（供测试/诊断）。
pub(crate) fn converge_float_finiteness(module: &inkwell::module::Module) -> bool {
    let mut rewritten = false;
    unsafe {
        let mut func = LLVMGetFirstFunction(module.as_mut_ptr());
        while !func.is_null() {
            if converge_function(func) {
                rewritten = true;
            }
            func = LLVMGetNextFunction(func);
        }
    }
    rewritten
}

unsafe fn converge_function(func: LLVMValueRef) -> bool {
    // 阶段 1：收集检查点
    let mut checks: Vec<CheckPoint> = Vec::new();
    let mut checked: HashSet<LLVMValueRef> = HashSet::new();

    let mut bb = LLVMGetFirstBasicBlock(func);
    while !bb.is_null() {
        let mut inst = LLVMGetFirstInstruction(bb);
        while !inst.is_null() {
            collect_check(func, inst, &mut checks, &mut checked);
            inst = LLVMGetNextInstruction(inst);
        }
        bb = LLVMGetNextBasicBlock(bb);
    }

    if checks.is_empty() {
        return false;
    }

    // 阶段 2：分类——中继点 vs 末端
    let mut to_rewrite: Vec<(LLVMValueRef, LLVMBasicBlockRef)> = Vec::new();
    for cp in &checks {
        if is_relay(cp, &checked) {
            to_rewrite.push((cp.cond_br, cp.ok_bb));
        }
    }

    if to_rewrite.is_empty() {
        return false;
    }

    // 阶段 3：重写 `cond_br` → 无条件 `br ok_bb`（检查部件变 dead，O1 DCE 清理）
    let builder = LLVMCreateBuilder();
    for (cond_br, ok_bb) in to_rewrite {
        LLVMPositionBuilderBefore(builder, cond_br);
        LLVMBuildBr(builder, ok_bb);
        LLVMInstructionEraseFromParent(cond_br);
    }
    LLVMDisposeBuilder(builder);
    true
}

/// 若 `inst` 是 SD-9 检查的条件分支（`br not_finite trap_bb ok_bb`，
/// `not_finite = or(is_nan=fcmp uno x,x, is_inf)` 且 trap 块非 fallible），
/// 登记检查点。
unsafe fn collect_check(
    _func: LLVMValueRef,
    inst: LLVMValueRef,
    checks: &mut Vec<CheckPoint>,
    checked: &mut HashSet<LLVMValueRef>,
) {
    // 只关心条件分支
    if LLVMGetInstructionOpcode(inst) != LLVMOpcode::LLVMBr || LLVMIsConditional(inst) == 0 {
        return;
    }
    let cond = LLVMGetCondition(inst);
    if cond.is_null() || LLVMGetInstructionOpcode(cond) != LLVMOpcode::LLVMOr {
        return;
    }
    // not_finite = or(is_nan, is_inf)
    let lhs = LLVMGetOperand(cond, 0);
    let rhs = LLVMGetOperand(cond, 1);
    let (is_nan, is_inf) = classify_fcmp(lhs, rhs);
    let is_nan = match is_nan {
        Some(v) => v,
        None => return,
    };
    // is_nan = fcmp uno x, x
    let x = LLVMGetOperand(is_nan, 0);
    if x.is_null() || LLVMGetOperand(is_nan, 1) != x {
        return;
    }
    // is_inf = fcmp oeq fabs(x), inf（容错：结构不完全时不识别为 SD-9 检查）
    let fabs_call = classify_fabs(is_inf);
    if fabs_call.is_none() {
        return;
    }
    // trap 块 = 分支的某个 successor；其中必须含 mimi_trap_float_not_finite
    // 调用（非 fallible）。fallible（Fault 吸收）不收敛。
    let succ0 = LLVMGetSuccessor(inst, 0);
    let succ1 = LLVMGetSuccessor(inst, 1);
    let (trap_bb, ok_bb) = if block_calls_trap(succ0) {
        (succ0, succ1)
    } else if block_calls_trap(succ1) {
        (succ1, succ0)
    } else {
        return; // fallible 或非 trap 分支，跳过
    };
    let _ = trap_bb;
    checks.push(CheckPoint {
        x,
        is_nan,
        fabs_call,
        cond_br: inst,
        ok_bb,
    });
    checked.insert(x);
}

/// 判断 `lhs`/`rhs` 是否为 `(fcmp uno x,x, fcmp oeq fabs(x), inf)` 的检查对。
/// 返回 (is_nan, is_inf)。
unsafe fn classify_fcmp(
    a: LLVMValueRef,
    b: LLVMValueRef,
) -> (Option<LLVMValueRef>, Option<LLVMValueRef>) {
    let (a_is_nan, b_is_nan) = (is_uno(a), is_uno(b));
    let (a_is_inf, b_is_inf) = (is_inf_cmp(a), is_inf_cmp(b));
    match (a_is_nan, b_is_nan, a_is_inf, b_is_inf) {
        (true, _, _, true) => (Some(a), Some(b)),
        (_, true, true, _) => (Some(b), Some(a)),
        _ => (None, None),
    }
}

unsafe fn is_uno(v: LLVMValueRef) -> bool {
    if v.is_null() || LLVMGetInstructionOpcode(v) != LLVMOpcode::LLVMFCmp {
        return false;
    }
    LLVMGetFCmpPredicate(v) == LLVMRealPredicate::LLVMRealUNO
        && LLVMGetNumOperands(v) == 2
        && LLVMGetOperand(v, 0) == LLVMGetOperand(v, 1)
}

unsafe fn is_inf_cmp(v: LLVMValueRef) -> bool {
    if v.is_null() || LLVMGetInstructionOpcode(v) != LLVMOpcode::LLVMFCmp {
        return false;
    }
    LLVMGetFCmpPredicate(v) == LLVMRealPredicate::LLVMRealOEQ
}

/// is_inf 的输入应为 fabs(x) 调用；返回该调用（其 operand 即 x）。
unsafe fn classify_fabs(is_inf: Option<LLVMValueRef>) -> Option<LLVMValueRef> {
    let is_inf = is_inf?;
    if LLVMGetNumOperands(is_inf) != 2 {
        return None;
    }
    let abs_val = LLVMGetOperand(is_inf, 0);
    if LLVMGetInstructionOpcode(abs_val) != LLVMOpcode::LLVMCall {
        return None;
    }
    Some(abs_val)
}

/// trap 块特征：含对 `mimi_trap_float_not_finite` 的调用。
unsafe fn block_calls_trap(bb: LLVMBasicBlockRef) -> bool {
    if bb.is_null() {
        return false;
    }
    let mut inst = LLVMGetFirstInstruction(bb);
    while !inst.is_null() {
        if LLVMGetInstructionOpcode(inst) == LLVMOpcode::LLVMCall {
            let callee = LLVMGetCalledValue(inst);
            if !callee.is_null() {
                let mut len = 0usize;
                let name = LLVMGetValueName2(callee, &mut len);
                if !name.is_null() {
                    let s = std::ffi::CStr::from_ptr(name).to_string_lossy();
                    if s == "mimi_trap_float_not_finite" {
                        return true;
                    }
                }
            }
        }
        inst = LLVMGetNextInstruction(inst);
    }
    false
}

unsafe fn is_float_type(v: LLVMValueRef) -> bool {
    let ty = LLVMTypeOf(v);
    let kind = LLVMGetTypeKind(ty);
    kind == LLVMTypeKind::LLVMDoubleTypeKind || kind == LLVMTypeKind::LLVMFloatTypeKind
}

/// 判断检查点 `cp.x` 是否为链中继点：
/// 所有用户（排除检查部件 is_nan / fabs(x)）都是"受检 f64/f32 代数 op"，
/// 或经无逃逸 alloca 转发后汇入链（store→load 对，load 消费皆是链成员）。
unsafe fn is_relay(cp: &CheckPoint, checked: &HashSet<LLVMValueRef>) -> bool {
    let mut had_real_use = false;
    let mut use_ref = LLVMGetFirstUse(cp.x);
    while !use_ref.is_null() {
        let user = LLVMGetUser(use_ref);
        // 排除检查部件自身
        if user == cp.is_nan || Some(user) == cp.fabs_call {
            use_ref = LLVMGetNextUse(use_ref);
            continue;
        }
        had_real_use = true;
        // 链成员：浮点算术指令且其自身结果也在检查点集合
        if is_chain_op(user, checked) {
            use_ref = LLVMGetNextUse(use_ref);
            continue;
        }
        // store 转发：x → store alloca → load → 链成员（alloca 无逃逸）
        if stores_into_forwardable_alloca(user, cp, checked) {
            use_ref = LLVMGetNextUse(use_ref);
            continue;
        }
        // 末端/观察点：保留检查
        return false;
    }
    // 零真实消费（结果 dead）→ 保留检查：检查分支使 fmul 保持 live，
    // 防止 LLVM DCE 掉表达式而丢失 trap 语义（E0813 是求值语义的一部分）。
    had_real_use
}

unsafe fn is_chain_op(user: LLVMValueRef, checked: &HashSet<LLVMValueRef>) -> bool {
    let opcode = LLVMGetInstructionOpcode(user);
    let is_arith = matches!(
        opcode,
        LLVMOpcode::LLVMFAdd
            | LLVMOpcode::LLVMFSub
            | LLVMOpcode::LLVMFMul
            | LLVMOpcode::LLVMFDiv
            | LLVMOpcode::LLVMFRem
    );
    let is_math_call = opcode == LLVMOpcode::LLVMCall && is_float_type(user);
    (is_arith || is_math_call) && checked.contains(&user)
}

/// store 转发判定：`user` 是 store 指令，所存值 == `cp.x`，目标为无逃逸
/// alloca（仅此一个 store + 若干 load），且所有 load 结果只被链成员消费。
/// 语义：x 非有限 → 经 alloca 转发流入链 → 汇入末端检查 trap，等价。
unsafe fn stores_into_forwardable_alloca(
    user: LLVMValueRef,
    cp: &CheckPoint,
    checked: &HashSet<LLVMValueRef>,
) -> bool {
    if LLVMGetInstructionOpcode(user) != LLVMOpcode::LLVMStore {
        return false;
    }
    // store 值必须是 x
    let stored = LLVMGetOperand(user, 0);
    if stored != cp.x {
        return false;
    }
    // 目标必须是指令（alloca）
    let ptr = LLVMGetOperand(user, 1);
    if LLVMGetInstructionOpcode(ptr) != LLVMOpcode::LLVMAlloca {
        return false;
    }
    // 扫描 alloca 的用户：1 个本 store（值即 x）+ 若干 load；其他 store
    // 仅允许常量初始化（entry 零值，循环内 store 覆盖后才被 load）；
    // 无其他逃逸（gep/ptrtoint/call 等拒绝转发）
    let mut loads: Vec<LLVMValueRef> = Vec::new();
    let mut use2 = LLVMGetFirstUse(ptr);
    while !use2.is_null() {
        let u2 = LLVMGetUser(use2);
        match LLVMGetInstructionOpcode(u2) {
            LLVMOpcode::LLVMStore => {
                if u2 != user {
                    // 其他 store：仅常量初始化允许（被本 store 覆盖语义安全）
                    let other_val = LLVMGetOperand(u2, 0);
                    if LLVMIsAConstant(other_val).is_null() {
                        return false;
                    }
                }
            }
            LLVMOpcode::LLVMLoad => loads.push(u2),
            _ => return false, // gep/ptrtoint/call 等 → 可能逃逸，不转发
        }
        use2 = LLVMGetNextUse(use2);
    }
    // 所有 load 结果只被链成员消费；loads 为空（结果 dead，store 后无人读）
    // 不转发——保留检查，防止 DCE 掉表达式丢失 trap 语义。
    if loads.is_empty() {
        return false;
    }
    for l in loads {
        let mut use3 = LLVMGetFirstUse(l);
        while !use3.is_null() {
            let u3 = LLVMGetUser(use3);
            if u3 == cp.is_nan || Some(u3) == cp.fabs_call {
                use3 = LLVMGetNextUse(use3);
                continue;
            }
            if !is_chain_op(u3, checked) {
                return false;
            }
            use3 = LLVMGetNextUse(use3);
        }
    }
    true
}