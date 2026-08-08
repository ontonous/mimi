# Changelog

## [Unreleased] — 0.1.5-dev

> 0.1.5 开发进行中：主线 = 性能优化（trap 成本消减 + O1 推进），质量次线见
> `devdocs/v0.35/README.md` 与 `devdocs/v0.34/dx-backlog-0.1.5.md`。

### 0.35.1 — 性能基线套件 + 四象限矩阵（0.1.5 首 sprint，Phase A）

> 0.1.5 性能主线的第一步：建立可复现基线，锁定 trap 成本分解的第一个靶子。
> 纯增量基建，零行为变更。基线报告 `devdocs/v0.35/perf-baseline-0.35.1.md`。

- **基准套件扩展**：`benchmarks/` 新增 dsp 热循环（一阶低通 5×10^7，dogfood
  M-014 同构场景）+ `dsp_ieee`/`mandelbrot_ieee` 变体（ieee_float 包裹悬浮
  SD-9 finiteness trap）+ `dsp.c`（gcc -O2 对照）/ `dsp.py`（CPython 对照）；
- **quadrant.sh**：四象限矩阵脚本——MIMI_OPT ∈ {0,1} × {默认, ieee_float}
  对 fib/mandelbrot/dsp 计时（纳秒括弧，RUNS=3 中位数）+ MIMI_DUMP_MODULE
  IR dump 静态 trap 调用点计数（trap 发射与优化档无关）；
- **基线矩阵（2026-08-08，32 核机）**：dsp O1 默认 402.1ms（3.97× C -O2）→
  O1+ieee 106.4ms（**1.06× 追平**）；mandelbrot O1 3.64× → ieee 1.88×；
  fib O1 2.88×（O0 3.68×）；
- **关键发现**：SD-9 float finiteness trap 占 dsp O1 默认耗时 **73.5%**
  （295.7ms，每 f64 binop 发射 NaN/Inf 检查）；整数 trap（SD-7）在 O1 下被
  LLVM 循环分析消除（ieee 版整数计数器仍 checked add 却追平 C）——整数
  trap 不是主要靶子；trap 静态计数与动态成本定性吻合（dsp 28→24 /
  mandelbrot 38→30）；
- **0.35.2 输入**：SD-9 链式末端检查假设（f64 代数链中间非有限必传播到
  末端，逐点检查可收敛为末端检查，语义保持）列入验证。

### 0.35.2 — trap 成本分解（perf 数据驱动，Phase A）

> 0.1.5 性能主线的第二步：perf 级分解锁定 SD-9 检查的真实成本结构，
> 完成 trap 语义分级表裁决。纯分析 sprint，零代码变更。
> 报告：`devdocs/v0.35/trap-decomposition-0.35.2.md`。

- **perf 分解（dsp 5×10^7）**：O1 默认 408.7ms vs O1+ieee 106.8ms
  （−73.9%）；指令 38.5→9.1/迭代，分支 4.05→0.015/迭代——**SD-9 检查的
  隐性成本 = 破坏 LLVM 向量化窗口**（每个 f64 binop 后的 finiteness 检查 +
  两路分支横插链中），向量化恢复是主要杠杆（ieee 版分支几近归零）；
- **链式末端检查假设验证成立**：IEEE 754 NaN/Inf 传播性论证——中间结果
  非有限必传播至链末端（NaN ⊛ x = NaN；Inf 参与结果 ∈ {NaN, ±Inf}），
  逆否等价；适用四条件：纯 binop 链 / 中间值未被比较·分支·内存写·调用消费 /
  非 fallible 上下文 / 非 ieee_float 块内；
- **trap 语义分级表裁决**：L1 链式末端检查（resolved+legacy+VM 三面对等）
  0.35.3 实施；L2 trap 分支 cold weight 顺带；L3 常量折叠先行；L4 整数 trap
  维持现状（0.35.1 实证 O1 已消除循环内 checked add）；**L5 nsw/nuw 放宽否决**
  （E0802 是语言承诺，改变可观测 trap 行为违反 L1 不变量）；L6 跨函数链
  识别 0.35.4 评估不承诺。

### 0.35.3 — SD-9 链式末端检查收敛（trap 成本消减 L1，Phase B）

> 0.1.5 性能主线的第一个实现 sprint：链收敛 pass 落地，dsp 追平 C -O2。
> 语义保持——trap 是语言承诺（SD-7/8/9），收敛只移动检查位置不改变可观测行为。

- **float_chain pass**（`src/codegen/float_chain.rs`，O1 管线前 LLVM IR 层）：
  收集全部 SD-9 检查点（`fcmp uno x,x` 特征指令）→ 被检值的所有用户（排除检查
  部件）都是受检 f64/f32 代数 op → 链中继点删除检查；末端/观察点（比较/存储/
  函数参数/返回/phi/不受检 op）保留；无逃逸 alloca store→load 转发入链（其他
  store 仅允许常量初始化）；[零真实消费（dead 结果）保留检查——防止 DCE 丢失
  trap 语义]；fallible 上下文（Fault 吸收）不收敛；
- **性能（四象限复测）**：dsp O1 默认 402.1→108.9ms（**3.7×，1.07× 追平
  C -O2**，较基线 3.97× 下降 73%）；mandelbrot O1 3.64×→1.80×（40.0→19.8ms）；
  fib 持平（无 float 链）；O0 档不变（收敛仅 O1 路径）；dsp 默认档与 ieee 档
  持平（108.9 vs 106.9）——链收敛达成 ieee_float 的全部收益且保留 trap 语义；
- **修复回归**：`ieee_depth_does_not_leak_across_function_boundary`——dead 结果
  的 fmul 检查被误删导致 Inf 偷偷通过（检查分支使 fmul 保持 live，删除后 LLVM
  DCE 掉表达式丢失 trap 语义）；修复为零真实消费 → 保留检查；
- **测试**：`src/tests/float_chain.rs` 探针 ×5（链中非有限末端 trap / dead 结果
  保留检查 / 比较观察点保留 / 有限链双端对等 / ieee 块消费边界）+ 既有
  ieee_depth 回归；全量 **5292 lib** 绿 / 15 real_world / 28 cli / clippy 零警告 /
  fmt 干净；诊断钩子 MIMI_DUMP_MODULE_CONVERGED（pass 后 dump）。

### 0.35.4 — trap 分支 cold 权重（trap 成本消减 L2，Phase B）

> 0.1.5 性能主线的收尾项：为全部 trap/Fault 分支附加 branch_weights cold
> metadata，让 LLVM 分支布局优化把 trap 代码移出热路径。检查合并/循环提升
> 经链收敛后无剩余空间（热循环已压到 1 检查/迭代），裁剪登记；CVP pass
> 实测无收益（fib 2.96× vs 2.98× 噪声内）不引入风险面，撤销。

- **mark_cold_trap_branch**（float_chain.rs）：`branch_weights {0,1}` cold 权重
  附加 helper（LLVMGetMDKindIDInContext + metadata_node）；5 处 trap 分支统一
  标记：SD-8 div-zero / MIN÷−1、SD-7 checked add/sub/mul、SD-9 float finiteness
  （legacy check_float_finite + resolved enforce_float_finite 两路）；
- **golden IR 更新 ×14**：branch_weights metadata 入 golden 快照（`!0 =
  !{!"branch_weights", i32 0, i32 1}`）；
- **CVP 评估**：pass 串实验 `default<O1>,correlated-propagation`——fib 的
  checked intrinsic 未被消除（alloca/load 模型下 range 分析不propagate），
  矩阵无收益（fib 47.3 vs 47.4ms 噪声内），按"无收益不引入风险"撤销；
- **检查合并/循环提升裁剪**：L1 链收敛后 dsp/mandelbrot 热循环均为 1 检查/
  迭代（环守卫/比较观察点，语义必需），无合并空间；环守卫不能提升出循环
  （非有限传播需实时捕获）；登记无剩余工作项；
- **验证**：全量 5292 lib 绿 / 15 real_world / 28 cli / clippy 零警告 / fmt 干净；
  矩阵终测 dsp 1.04× / mandelbrot 1.78× / fib 2.90×（O1）。

### 0.35.5 — nsw/nuw 语义分级评估（裁剪登记，Phase B）

> Phase B 可选工作项评估后裁剪：SD-7/8/9 trap 是语言承诺（E0802/E0801/E0813），
> nsw/nuw 放宽会改变可观测 trap 行为，违反 L1 不变量（0.35.2 已否决）。
> 无代码变更，仅为路线图/预算完整性登记。

### 0.35.6 — 四象限矩阵终测 + 曲线报告（Phase B 收官）

> Phase B（trap 成本消减）终测：链收敛 + cold 权重双項落地后的完整矩阵复测
> 与收敛曲线报告。

- **终测矩阵（O1，2026-08-08）**：dsp 402.1→104.5ms（**1.04× 追平 C -O2，降 74%**）；
  mandelbrot 40.0→19.8ms（1.78×，降 50%）；fib 持平 2.90×（递归调用 + checked
  intrinsic 组合开销，登记 0.1.5 范围外——需 emitter 架构级优化）；O0 全程不变
  （收敛/cold 权重仅 O1 路径）；
- **曲线**：trap 主导项（dsp）成本降 74% ≥ 30% 验收达成；dsp 默认档与 ieee 档
  持平（104.5 vs 103.9ms）——链收敛 + cold 权重达成 ieee_float 全部收益且完整
  保留 trap 语义；**Phase B 关闭**。

### 0.35.7 — strings/collections 模块体进 resolved slice（Phase C）

> dx-backlog #19：strings/collections 模块体（含 trait impl 方法体）进 resolved
> slice。根因是 str_* builtin 的 {ptr,i64} 值 → runtime-direct ptr 的 coercion 失败
> 拖垮所有调用它们的 stdlib 函数体。顺带修复了三个被真实程序暴露的既有 bug。

- **STRING_ABI_BUILTINS**（resolved/mod.rs）：16 个 str_* builtin 跳过
  runtime-direct 快捷路径，强制走 string emitters（compile_builtin_call）——
  `str_char_at` 等 runtime helper 收 raw C-string ptr，而 resolved 传 Mimi
  string 值 {ptr,i64}，coercion 失败导致每个调用它的 stdlib 函数体 resolved
  编译失败落 legacy（0.34.38 只修了 str_substring 单点）；
- **默认白名单扩展**（eligibility.rs `module_bodies_lifted`）：
  prelude,mymath → +strings,collections。A/B 语料五程序（std_strings /
  std_collections / 06_strings / 05_lists / json_parser）编译 + 运行全过，
  dispatch 统计 eligible 34/45、零 module skip（legacy 11 全为
  generics/qualified）；
- **修复 1 — mimi_runtime_assert 缺失**（runtime/mod.rs）：legacy pattern
  binder（func/pattern.rs `PatternKind::Literal`）早就在调用 `mimi_runtime_assert`
  (bool, ptr)，但 runtime 从未定义该符号——任何带 literal 子模式
  （`Bool(true) =>`）的程序链接失败。补实现（E0801 家族：失败打印 + abort）；
- **修复 2 — 泛型参数名遮蔽用户类型**（func.rs `legacy_param_llvm_type`）：
  `type T` + prelude `eq<T>` 时，legacy 编译泛型骨架把参数 T 解析成用户 enum
  的 struct 布局，`a == b` 报 "eq requires same types"（E0700）。declare_func /
  bind_func_params 统一走新 helper：泛型参数名 → i64 占位（骨架仅满足 legacy
  声明 pass，真实调用走 monomorphize）；
- **修复 3 — literal 子模式 fall-through**（expr/match.rs）：Constructor arm
  只在 tag 匹配时进入，payload literal 比较被推迟到 pattern binding——
  `B(false)` 值落到 `B(true)` arm 时 assert abort 而非落入下一 arm。修复：
  literal 字段比较并入 arm 条件（tag AND payload）；
- **回归测试**：real_world_literal_pattern_fallthrough（双后端，覆盖修复 2/3）；
- **验证**：全量 5292 lib 绿 / 16 real_world / 29 cli / clippy 零警告 / fmt 干净。

### 0.35.8 — fails transition Result 布局对齐（Phase C）

> dx-backlog #20：flow_order_system native SIGSEGV（`puts(0x1)` 整数当字符串
> 指针，gdb 实证）。fails transition 返回 Result<Target,(Source,E)> 中 string
> 字段布局错位——0.34.25a 只修了 Err 臂（Q1），Ok 臂 + 事件参数路径未修。
> 两个独立根因，双双修复。

- **修复 1 — Ok payload coach load**（func.rs `coerce_field_to_type`）：
  `compile_ok_constructor` 打包 `{i1, ptr, i64}`（payload 槽存目标地址），
  coerce 到声明布局 `{i1, T, E}` 时把裸指针 store 进 T 槽位——struct payload
  （含 string 字段的 Flow state）地址位被当字段读。新增 ptr→struct 分支：
  build_load 解引用（mimi-string wrap 分支在前保持 C-string 指针语义）；
- **修复 2 — flow transition 字符串字面量参数**（method.rs
  `compile_flow_transition_call`）：字符串字面量参数编译为 raw global-string
  指针，旧代码按参数类型 `{ptr,i64}` 直接 load——把字符串自身字节读成
  {data_ptr, len}（"TXN-42" → data_ptr=0x32342D4E5854）。改为 wrap_c_string
  （非 string struct 参数仍 load）；
- **回归**：flow_order_system + flow_system_trace 从 dual-backend
  known_limitations 移除（两个 SIGSEGV 均修复），纳入双后端套件；
- **验证**：flow_ 365 测试全过；全量 5292 lib 绿 / 15 real_world / 29 cli /
  clippy 零警告 / fmt 干净；flow_order_system native 输出与 VM 逐行一致
  （TXN-42/TRK-001/book/invalid price/0）。

### 0.35.9 — contracts 守卫发射性能切片（Phase C）

> 0.34.41 第二档（resolved 运行时守卫发射）第一个性能回访。结论：
> **守卫成本 < 噪声，零优化空间**。

- **基准**：`benchmarks/contracts.mimi`——`validated_sum(n, lo)` 带双合约
  （requires n>=lo + ensures result>=0），300 万次调用（参数 k%20 防
  LICM 提升、lo 来自 argv[1] 防常量折叠）；`plain_sum` 同构无合约对照；
- **数据（O1，3 次中位数）**：擦除 0.02s vs `--verify-contracts` 0.02s——
  守卫被内联 + 分支预测近全命中，双向无差异；
- **IR 验证**：requires/ensures 的 icmp + contract_pass/fail 双块 +
  mimi_runtime_abort（E0808）完整存活，与 0.34.41 设计一致；循环内 checked
  add 的 trap_overflow + branch_weights（0.35.4）与守卫共存；
- **登记**：无优化空间（重守卫 = 用户表达式成本，非机制开销，不预优化）；
  报告 `devdocs/v0.35/contracts-slice-0.35.9.md`；
- **验证**：全量 5292 lib 绿 / clippy 零警告 / fmt 干净。

### 0.35.10 — dispatch 门禁复测 + 覆盖率曲线（Phase C 收官）

> Phase C 收官：全语料 113 程序（demos + examples + tests/real_world）
> dispatch 复测与覆盖率曲线。报告 `devdocs/v0.35/dispatch-coverage-0.35.10.md`。

- **曲线**：0.9609（0.34.40 门禁建立）→ 0.3027（0.34.42 slice 放开）→
  **0.2735**（0.35.7 strings/collections 模块体进 slice，−0.0292）；
  eligible 202 → 3783 → **3974**（72.65%）；
- **复测**：113 程序全部编译成功（0 emit_failed）；门禁 check 无静默回退
  （0.2735 < 0.3027）；新增程序 json_parser 自动纳入（0.2667）；
- **基线更新**：dispatch-baseline.json 更新至 3974/5470（0.2735）；
- **剩余拆解**：legacy 1496 中 generics/qualified 1331（**89%**）——泛型函数
  进 resolved slice 需单态化/泛型 emit（架构级），**登记 1.x 评估**；
  module file 81（io/fs/net 未 lift 模块，登记 0.35.x 后续）；unsupported
  type/expr 62；actor/nominal 15；
- **验证**：全量 5292 lib 绿 / clippy 零警告 / fmt 干净。

### 0.35.11 — O1 正确性切片：O1/O0 双档对等三连修（Phase D）

> O1 默认化后的 dual 对等扫描发现 `demos/05_lists.mimi` 双档分歧：O0
> tcache double free abort、O1 静默输出错。三连根因各自独立，修复后
> 05_lists O0/O1 与 bytecode 逐行对等（除 zip 行，见已知限制）。

- **修复 1：resolved slice trap 块缺 terminator**（`src/codegen/resolved/mod.rs`
  `emit_bounds_trap`）：slice OOB trap 块以 noreturn call 结尾但无 terminator
  → LLVM verify 拒绝整个函数 → **静默降级 legacy emitter**（连带 print
  dispatch 错乱）。补 `build_unreachable`；`sort(data)` + `data[1..4]` 同函数
  复现；
- **修复 2：legacy print dispatch 对 list 返回内建的类型推断缺口**
  （`src/codegen/expr.rs` + `func.rs` + `block.rs`）：`map`/`filter`（编译期
  内建）与 `reverse`/`sort`/`range`（无 func_defs 条目的运行时 builtin）的
  调用结果被推断为 **被调名**（"map"、"reverse"…）或空 → `{i64 len, ptr data}`
  list struct 误入字符串快速路径（printf 对结构体字节 strlen，输出
  空/垃圾，O0 下可触发 double free）。新增共享 helper
  `infer_list_builtin_return_type`（从源参数推导 List<T>；map 优先取 lambda
  声明返回型），接入 `infer_object_type` Call 分支 + 两处 let 绑定追踪；
  另补 `SliceExpr` arm（`xs[1..5]` 同源类型）；**zip 不入 helper**：其裸
  {i64,i64} pair 布局与 product-tuple formatter 的 heap-pack 假设不符，
  强类型化会 segfault（已知限制：legacy 下 zip 显示为空，O1 不 crash）；
- **修复 3：list 字面量绑定 local 后 realloc 所有权陈旧**（resolved，
  `src/codegen/resolved/mod.rs`）：`let mut ys = [1,2,3]; push(ys, 4)` 先在
  构造临时 alloca 建表并注册为 buffer 所有者，再值拷贝进 local；push/pop
  的 realloc 更新的是 **local** 槽，注册槽残留 realloc 前旧指针 → scope
  退出 free 旧指针（realloc 搬移时已内部释放）→ tcache double free。
  O1 仅因 SROA 合并两 alloca 侥幸不炸。新增 `emit_list_literal(target)`
  直接构造模式 + Bind 快速路径：字面量直接绑定简单 local 时就地构造，
  注册所有者 = 被 mutator 更新的槽；
- **诊断钩子**：`MIMI_DUMP_MODULE` 提升到 optimize gate 之外（O0 构建
  也可 dump IR，此前 O1-only 位置使默认 debug opt-out 构建不可见）；
- **回归锁**：`real_world_list_intrinsic_display_and_realloc`（sort/slice/
  reverse/map/filter 内联+绑定显示 + push/pop realloc 所有权，run/build
  双后端对等）；临时复现文件 dblfree_min.mimi 已转正删除；
- **验证**：05_lists O0/O1 与 bytecode 输出逐行对等（zip 行除外）；全量
  5292 lib + 29 real_world + cli 套件绿。

### 0.35.12 — resolve→zonk 全量迁移 + parser panic 审计第一批（Phase D）

> dx-backlog #1 关闭 + #2 第一批（parse_expr/parse_stmt）审计落地。
> 两半各自独立提交（0.35.12a 迁移 / 0.35.12b 审计）。

- **#1 resolve()→zonk_or_unknown() 全量迁移（31 处生产调用点清零）**：
  infer_expr×2 / check_stmt×4 / checker·func×1 / checker·vars×6 /
  checker·items×2 / checker×1 / infer·lambda×1 / infer·call·helpers×5 /
  infer·call·simple×5 / infer·helpers×1 / infer·record×3。**语义裁决**：
  迁移点均为推断内部（非定稿边界），游离 TypeVar（let 多态占位）/ForAll/
  逃逸哨兵在迁移前 resolve 中原样透传——首版直接套 scan_residual 严格化
  在 flow checker unknown 哨兵与 let 多态游离变量两类合法路径上触发
  debug 断言（实证后回退）；`zonk_or_unknown` 最终对齐 pre-migration
  resolve 语义（resolve_infer + unknown 兜底 + 可见 debug 断言），严格
  定稿仍由真边界处的 `zonk` 承担；`resolve()` 降为 #[deprecated] 转发，
  仅 unification.rs 模块测试消费；
- **#2 parser panic 审计第一批（parse_expr/parse_stmt）**：审计结论矫正
  登记口径——**51 处 `panic!` 全部位于 #[cfg(test)] 测试区**（原登记未
  分离测试代码）；`unwrap()/expect(` 计数含 parser 自身的 Result 返回
  `self.expect()` 辅助方法（非 Option::expect）。生产区真实残留仅 3 处
  结构性不变量 unwrap（if-chain 首链 ×2 + token 索引 ×1），全部降级为
  `ParseError` 诊断（用户输入永不 abort 编译器）；
- **验证**：5292 lib 全绿（含 parser 92 / property 44）；clippy 零警告；
  fmt 干净。

### 0.35.13 — trivia 化：desc/rule/mms{} 降注释，Stmt 33→30（Phase D）

> dx-backlog #10（0.34.5a 推迟项）落地：`desc`/`rule`/`mms{}` 从 AST 降为
> trivia——parser 消费即弃（验证括号结构但不产出语句），表面语法兼容
> （旧源码继续可解析）。`math:` 保留 verifier 通道（P1 裁决）。#13（actor
> runs_flow 三层集成）从本 sprint 拆出单独排期（曾有“超单 sprint 范围”
> 回滚史）。

- **parser**：`parse_stmt_kind` 删除 Desc/Rule/Mms 产出臂（块外出现报
  trivia 诊断）；两个 block 循环（terminator/recovery）改为消费即弃；
  `parse_mms_block` 改返回 `()`，删除文本重建逻辑（仅保留括号平衡消费）；
- **AST**：`Stmt::Desc`/`Stmt::Rule`/`Stmt::MmsBlock` 三 variant 删除
  （Stmt 33→30）；
- **消费面清理（82 处 / 22 文件）**：resolved（语义键/节点标签/span 抽取
  7 臂）/ checker（check_stmt×3、func 合约探测、borrow）/ codegen（block/
  func/actors skip 臂）/ bytecode VM / CFG lower / resolved IR lower /
  loader span remap / lint / verifier×4 文件 skip 臂；
- **真实依赖处置**：`doc_core` desc/rule 文档提取循环删除（降注释后无
  结构化意图文本可提取，属裁决内行为）；`core::verify_rules` 降为恒净
  no-op（保留 CLI `--verify-rules` 接口）；LSP `has_contracts` 探测去
  MmsBlock；
- **测试重写**：`mms_integration.rs` 8 项全重写为 trivia 契约（解析无错 +
  零 AST 语句 + 运行时语义不变，新增独立 desc/rule trivia 锁）；
  parser/flow.rs mms 嵌套括号测试改为“消费后仅余 return”；语料实测
  desc/rule/mms{} 使用量为零（demos/examples/tests/std/projects 全扫），
  且新测试发现登记口径误差：真实语法为 `desc "text"` 无冒号；
- **验证**：全量 5292 lib + 30 real_world + cli 绿；clippy 零警告；fmt 干净。

### 0.35.14 — DX backlog 三项：#13 runs_flow 三层集成 + #16 C stdio 混流 + #18 tuple fn 取出（Phase D）

> 质量次线三项落地。头条是 #13（0.34.29 曾有"超单 sprint 范围"回滚史）：
> actor `runs` Flow 的 transition 方法调用（`a.inc()`）此前被 checker
> 误报 E0221 "has no method"（合法程序被拒 = L2 假阳性），本次实测逐层
> 推进完成类型检查层三层集成；`mimi check` 全通，bytecode dispatch 运行时
> 行为不变。#3（LSP Span/Origin 迁移）顺延下个 sprint。

- **#13 层①（checker + infer 方法注册）**：`collect_item_decls` 将 runs_flow
  actor 的每个 transition 注册为合成方法（签名 `(self, event params…) ->
  ToState`；`fails E` 时返回 `Result<ToState, (FromState, E)>`——与
  codegen/VM dispatch 消费的形状一致）；显式同名方法优先。infer 新增
  `runs_flow_transition` 辅助 + `is_actor_method` 扩展：调用点按 actor 方法
  dispatch（E0221/E0257 误报消除），参数类型/元数照常 E0211/E0257，typo
  建议含 transition 名；
- **#13 层②（zonked 签名）**：无需额外注册——`finalize_zonked_func_types`
  遍历 checker `funcs` 目录，自动覆盖合成条目；
- **#13 层③（resolved callable identity）**：resolved 目录为每个 transition
  注册 `function:{Actor}::{transition}` 的 `ResolvedFunction`（含 implicit
  self + 参数 NodeMeta）与 `ResolvedActorMethod`（call-site KIND 事实因此
  归类为 Method 而非 Unknown）；typed body lowering 对合成 callable 豁免
  （无语义体，转移表由 VM/codegen 运行时 dispatch）。**边界**：`mimi build`
  codegen 仍报 E0700（runs_flow codegen 维持 0.2 登记，spec §6.12）；
- **#16（C stdio 混流乱序）**：VM `print`/`println`/`print_err` 每次 Rust
  侧写前 `fflush(nullptr)` 抽干 C stdio 块缓冲——比"退出前单次 flush"更强，
  在每个交错点保持程序序；无 FFI 场景空缓冲 flush 为廉价 no-op；
- **#18（tuple fn 元素取出调用）**：tuple 字面量中的具名函数元素在绑定处
  登记（`record_tuple_fn_elems`/`register_tuple_index_fn_binding`，Stmt::Let
  双路径），`let f = t.0` 取出绑定走间接调用路径（i64 ptrtoint 槽 inttoptr
  还原指针）——此前 codegen 报 E0700 "undefined function 'f'"（VM 可执行，
  双后端分歧）；
- **回归**：actors.rs 新增 3 项（dispatch typed check + 参数类型 E0211 +
  fails Result 形状），real_world 新增 `real_world_tuple_fn_element_call`
  双后端锁；全量 5294 lib + 31 real_world + cli 绿；clippy 零警告；fmt 干净。

### 0.35.15 — LSP 文本搜索 → Span/Origin 迁移（dx-backlog #3，Phase D 顺延项）

> A6 基础设施（Span/Origin/AstNodeMeta + PositionMap）的消费端收尾：LSP
> 内全部“文本扫描定位 AST 位置”的调用点迁移到 AST span（探针测试锁定锚点
> 契约后逐文件迁移）。顺带修掉一批潜伏假阳性：`contains("impl")` 落在
> 首个 impl、`func {name}` 落在注释/调用点、let 绑定落在同名先行绑定、
> 括号计数被 `let s = "}"` 截断。

- **探针测试**（`src/tests/lsp.rs` +2 项）：锁定迁移依赖的 span 锚点契约
  ——FuncDef/TypeDef/ModuleDef/ImplDef 锚定关键字、let pattern 锚定绑定名、
  Call 表达式锚定 callee、FuncDef.end_line 到闭括号行；
- **references.rs**：goto-definition/references/highlight 的 Type/Module/
  let 绑定定位全部 span 化（删除死文本回退）；impl 跳转位置精确到块行；
  `enclosing_func_line_range` 改 AST 包含（rename 作用域不再依赖
  `starts_with("func ")` 启发式）；
- **util.rs**：`find_func_end_line` 删除（SourceScanner 括号计数被
  `span.end_line` 平替），`find_enclosing_func_in_items`/`hash_func_body`
  span 化（签名去 text 参数）；
- **symbols.rs / lens.rs / hierarchy.rs / inlay.rs**：文档符号/工作区符号/
  code lens/调用层级/inlay 提示的 def-line 与 call-line 全部 span 化；
  inlay 参数提示的 call-line 不再落在首次提及，括号扫描从 callee 名尾开始；
- **测试更新**：audit_fix_lsp 括号计数测试重写为 span 包含测试（字符串/
  注释内括号假目标结构性免疫）；全量 5296 lib + 31 real_world + cli 绿；
  clippy 零警告；fmt 干净。

## [0.1.4] — 2026-08-08

> **语法冻结 + 语义裁决落地 + 架构冻结（Phase G）**。
> 0.1.4 里程碑：become/stay 删除（ADR-001）、multi-target 稳定 tagged-union ABI
> （ADR-002）、`'a` 删除（ADR-004）、`do` wrapper 删除（关键字 81→80）、and/or/not
> 软关键字化、if let / for 解构、`ieee_float {}`、单向数值强制、View/Mutate 闭合、
> O1 默认优化、诊断契约（diagnostics.md）。Phase G 架构冻结：ADR-005~008 四项正式
> 裁决、resolved dispatch 度量门禁（fallback_rate 0.9609→0.3027）、contracts 与
> stdlib 模块函数体进 resolved slice、view/mutate 借用参数 ABI 对齐、verifier 引擎
> 隔离（E0439）、ABI 布局冻结 + abi_version 握手（native-abi-1 §7/§8）、pre-0.1 更名、
> 0.minor=大版本战略。

### 0.34.46 — 0.1.4 全面查缺补漏（登记面 / 记录面 / 代码面）

> 0.1.4 收尾后对全部开发内容做系统性查缺补漏：登记完整性、记录完整性、
> 代码卫生三面清理。无行为变更（探针验证 + 新增回归锁）。

- **登记面**：dx-backlog 补 #19（strings/collections 模块体 trait 符号路由阻塞，
  0.34.42 裁剪）+ #20（flow_order_system fails transition SIGSEGV，0.34.45）；
  审计台账 §11-#45 回写 ✅（0.34.44 ADR-008 已闭：LSP 缓存键引擎隔离 + 双引擎
  fail-closed）；台账新增 §12.1 移交登记表（8 项无去向 🔴 统一登记轨：§2-#20/
  §4-#39/§4-#43/R-3/R-5/G-3/§1-#11/§13-#73）；
- **记录面**：golden-document §10 补 Phase G 表（0.34.34-45 全量完成状态，此前
  只到 0.34.33）；0.34.5a trivia 化排期行补 ⬜ 推迟 0.1.5 回写；0.34.3 的
  "codegen E0700 登记缺口"实测已闭回写；CHANGELOG 补 0.34.1-23（Phase A–E）
  合并条目（此前只到 0.34.24，发布需完整）；README 双语 0.1.4 行同步 Phase G
  交付 + RC 测试数 4598→5285；
- **代码面**：E0439 注册补全（codes.rs 常量 + describe 表，此前 verifier 硬编码
  未登记）；码集合补 E0431（0.34.10 引入时漏加）+ E0439；5 处 `e2e_asan_*` 裸
  `#[ignore]` 补裁决注释（§13.15 "0 未登记 ignore" 纪律）；if let / for (k,v)
  解构 native codegen 实测已闭（探针 p13 双端对等，main 走 resolved）——补
  dual 回归锁 ×2（dual_if_let_and_tuple_for_destructuring /
  dual_if_let_none_arm_skips）；
- **验证**：全量（含新 dual 测试）绿；clippy + fmt 干净；探针 p13 双端对等。

### 0.34.45 — AF-2 ABI 定稿 + pre-0.1 更名战役 + 0.1.4 RC 复核（G3 硬复核点）

> Phase G 第七个 sprint（收尾）。AF-2 布局冻结定稿 + abi_version 握手登记、
> pre-1.0 → pre-0.1 更名战役、0.minor=大版本战略、Phase G 验收清单全绿。
> **Phase G 至此全部关闭**（AF-1/2/3/4 + 度量门禁 + 三假边界 + 引擎隔离）。

- **① ABI 布局定稿**（ADR-007 落地）：`docs/spec/native-abi-1.md` 新增 §7 布局冻结
  声明（string `{ptr,len}` 无 capacity / list `has_header` 显式标志禁裸读
  `data[-1]` / handle tag 位永驻 / nominal handle slot+generation）+ §7.2
  “布局内解决”约束清单（**A2** 指针往返 loss → handle 化路径禁 ptr↔int；
  **A3** tag 永驻是正式语义；**B10** has_header fail-closed）；
- **② abi_version 握手登记**：`ComponentIdentity.abi_version`（当前值 **1**）与
  本文件布局绑定；1.x 布局变更（胖指针/tag 剥离/capacity）→ 新 version，旧组件
  以旧版本继续加载——**Two-Way Door**（0.2 随 bindgen 回归铁律实施）；
- **③ pre-1.0 → pre-0.1 更名战役**：git 跟踪文件 pre-1.0 **零残留**（grep 门禁：
  `git grep "pre-1.0" -- ':!devdocs' ':!CHANGELOG.md'` = 0）；spec 同步闸
  `check_language_docs.py` 指向 `devdocs/pre-0.1`（修掉 glob 扫空脱闸）；
  docs×6 + README×2 + codes.rs 注释 + devdocs 路径引用全清（CHANGELOG
  历史条目豁免）；
- **④ 0.minor=大版本战略入 AGENTS §13.1.1**：minor 即大版本边界（0.1.x 是 1.0
  的 pre-阶段）、每 minor 冻结点表、breaking 政策（1.0.0 起需 major）、迁移
  注记纪律（无迁移注记的 breaking 禁止合入）；
- **⑤ Phase G 验收**：全门禁复跑通过——lib 全量 5285 绿 / clippy 零警告 / fmt 干净 /
  `check_language_docs.py` 31+31 绿 / dispatch 门禁 0.3027 无回退 / unsafe gate 通过；
  fallback 率曲线收尾（0.9609 → 0.3030 → 0.3027，eligible 212 → 3783）；
- **登记 0.1.5（real_world 存量缺陷，非本次回归）**：`flow_order_system.mimi`
  native 产物 SIGSEGV（`puts(0x1)` 整数当字符串指针）——2026-07-27 `9d4f17f3`
  已登记同族缺陷（string event parameter + fails transition → SIGSEGV，
  VM run 正常）；0.34.44 后 codegen/core 零改动（diff=0）证明存量；
  `flow_system_trace.mimi`（同形态无 fails）PASS 锁定差异面在 fails 路径。
  修复排 0.1.5 codegen 轨；
- **工具**：`dispatch_stat.py` 开头清理容错修复（残留时 rmtree 竞态抛错）。

### 0.34.44 — verifier 引擎隔离治理（AF-3 落地，ADR-008）

> Phase G 第六个 sprint。resolved 主引擎 + 缓存引擎隔离 + 双引擎分歧
> fail-closed；VIR（flow_ast）降级为 math: 通道，退役登记 0.2 轨。

- **引擎身份入缓存键**：`ProofArtifact` 新增 `engine` 字段
  （`ENGINE_FLOW_AST`/`ENGINE_RESOLVED`）；`cache_key()` =
  (semantics, solver, integer_model, **engine**, program_identity)，
  resolved 引擎绑 resolved_ir_hash、flow 引擎绑 vir_hash；`is_compatible`
  含引擎相等——跨引擎证明复用结构上不可能（fail-loud cache miss）；
- **LSP 只读 resolved 引擎缓存**：验证缓存键统一走
  `verification_cache_key`（uri + func + resolved + 语义版本），读写
  同源；旧格式磁盘缓存（无引擎段）永不命中，升级自动失效；
- **双引擎分歧 fail-closed**：`mimi verify` 主路径改用
  `verify_checked_dual`——resolved（主判定）+ flow/VIR 并跑，裁决类
  （Proven/Disproven/Inconclusive/NoOpinion）分歧时取较弱结论 +
  新诊断码 **E0439**（注册 error-codes.md）+ artifact 作废；
  NoOpinion（未尝试证明）不构成分歧；仅 flow 覆盖的义务（如 call-site）
  透传不丢；
- **实测分歧（诚实披露）**：两引擎整数模型不对称——flow 对 i64 强制
  溢出定义性而 resolved 按无界模型证明（i32 方向相反）；存量算术合约
  （examples/validation_contracts.mimi 的 fib/factorial/divide/withdraw）
  现报 E0439 + fail-closed，两引擎整数模型统一登记为 VIR 退役前置
  条件（0.2 轨，diagnostics.md G8）；
- **验证**：回归锁 ×8（缓存键引擎隔离/兼容性/LSP 键形/分歧合并四
  向/NoOpinion 让位/flow-only 透传）+ Z3 端到端 ×3（一致路径、i64/i32
  双向分歧 fail-closed）；全量 5285 绿；clippy 零警告；

### 0.34.43 — view/mutate 借用参数 ABI 对齐进 resolved slice（AF-4 前置 2③）

> Phase G 第五个 sprint，AF-4 三个假边界全部落地。非-self 标量 view/mutate
> 借用参数进 resolved slice，指针 ABI 与 legacy 完全对齐；曲线点
> fallback_rate 0.3030→0.3027（eligible 3781→3783）。

- **ABI 对齐**：`declare_callable` 对非-self 借用参数发 ptr 参数类型（对齐
  legacy `declare_func` 的 `param.borrow.is_some() → ptr`）；`bind_parameters`
  借用参数直用入参指针作 storage（不新建 alloca——callee 存储即 caller
  存储，真引用语义，无写回步骤，对齐 0.34.13）；
- **调用传址**：Call arm 按 callee signature 逐位识别借用参数，实参仅接受
  裸 `Load(place)`（无投影、conversion=Identity）时传 caller 存储地址；
  其余形态 fail-closed per-function 降级 legacy（不静默 ABI 错配）；嵌套
  借用转发（借用参数再传入借用参数）指针透传不重建；
- **切片纪律**：eligibility 删除 0.32.20 拒绝；`require_scalar_type` 继续
  守住 record/List 借用（`mutate Buffer` 等仍留 legacy，待指针投影/间接
  存储支持后评估）；self 接收者保持 value ABI 例外；
- **验证**：探针 p11/p12 双端对等（mutate 写回可见 / view 只读 / 嵌套转发，
  34/34 全函数转正）；dual 回归 ×3（`dual_borrow_mutate_scalar_writeback` /
  `dual_borrow_view_scalar_read` / `dual_borrow_forward_nested`）；全量
  5274 绿（含既有 view_mutate_exec/mutate_field_writeback 条款 6 族）；
  clippy 零警告；A/B 默认模式全语料零回退；

### 0.34.42 — stdlib 模块函数体进 resolved slice：source_id workaround 根因化（AF-4 前置 2②）

> Phase G 第四个 sprint。历史 SIGSEGV（0.32.8 std_strings/multiple_std_modules）
> 当年用 source_id 过滤器整体屏蔽了 3848 个模块函数实例——本 sprint 实测根因
> 并逐 slice 放开。**曲线点：fallback_rate 0.9609 → 0.3030**（eligible
> 212→3781，+3569 转正，AF-4 迄今最大单步覆盖率收益）。

- **根因实测**（与 AF-4 草案预判同框架）：gdb 抓崩在 LLVM
  `LowerExpectIntrinsicPass`；`MIMI_DUMP_MODULE` 导出 IR 发现无终止符残桩函数。
  链路：resolved emitter 发射失败后残桩留在模块 → legacy 重编的 skip 守卫用
  surface `func.name` 查 `resolved_failed_functions`，而失败登记用 catalog
  `qualified_name`（impl 方法两者不同，如 `char_at` vs `string_char_at`）→
  查不到 → 残桩当“已编译”跳过 → 无效 IR 崩 pass pipeline；
- **修复 ×3**：① `compile_subset` 两个失败分支（verify 失败 / emit Err）现场
  清空残桩块（`clear_partial_body` 还原纯声明，符号保留供调用方引用）；
  ② legacy skip 守卫改 `func.name` + LLVM 实际符号名双键查找；③ source_id
  过滤器改白名单门 `module_bodies_lifted`（env `MIMI_RESOLVED_MODULE_BODIES`：
  `=1` 全解除实验模式 / `=csv` 片段覆盖 / 未设=内建白名单 `prelude,mymath`，
  按 `<frag>.mimi` 路径尾匹配防误放）；
- **A/B 证据**（120 程序双模式 build+run 对比）：prelude 零回退 → +mymath
  零回退 → +strings 回退 1（a1_verification 链接 undefined symbol
  `string_char_at`——resolved trait 调用方指向的符号与 legacy 方法体所在
  mangled 符号不同，trait dispatch 符号路由问题，显式裁剪登记 0.1.5）；
  默认模式全语料 equivalent 101/120（其余为两侧同态既有噪音）；
- **曲线纪律**：基线 dispatch-baseline.json 重生（3781/5425，0.3030），
  门禁只降不升；覆盖率曲线单调不降 + 全量零新增失败即验收（允许部分模块，
  strings/collections 登记 0.1.5）；

### 0.34.41 — contracts 进 resolved slice：默认擦除转正（第一档）+ 运行时守卫发射（第二档）（AF-4 前置 2①）

> Phase G 第三个 sprint。安全分档策略：**第一档**让带合约函数在默认路径
> （verify_contracts=false，合约擦除）脱离 legacy；**第二档**把运行时守卫发射
> 接入 resolved emitter，解除 --verify-contracts 的 fail-closed。两档均已落地。

- **第一档落地**：`require_resolved_native_callable_with_source` 加
  `verify_contracts` 门——verify_contracts=false（默认）时带合约函数进 resolved
  （resolved emitter 的 `Contract` arm 已为 no-op，与 legacy 默认擦除对齐）；
  --verify-contracts 时 fail-closed 到 legacy（守卫仍由 legacy 发射，不静默丢守卫）；
  all-or-nothing 路径保守不变（`require_resolved_native_callable` 传 true）；
  传参链：`eligible_function_ids_with_stats`/`resolved_eligible_functions`/compile.rs
  各加 verify_contracts 形参；
- **fallback 率差值报告**：eligible 202→212（+10 带合约函数全量转正），聚合
  fallback_rate 0.9628→0.9609（−0.0018，改进方向）；contracts skip reason 清零；
  基线 dispatch-baseline.json 更新至 212/5425；
- **dual 回归 ×2**：`dual_contract_fn_erased_default_runs_on_resolved`（合约擦除下
  多合约函数交互双端对等，无错值）+ `dual_contract_requires_violation_traps_both_
  backends`（--verify-contracts 违反 requires 双端 E0808 trap 对等）；新增 helper
  `dual_assert_contract_violation`（断言 VM + codegen 都 trap 且含 E0808）；
- **门禁**：全量 5268 lib 绿（+2 新测试）/ clippy -D warnings 绿 / fmt 干净 /
  语料零 SIGSEGV（112 程序编译成功）；
- **第二档落地（resolved 守卫发射）**：`emit_contract_prologue`（入口 old() 快照
  按 Old 节点 NodeId 键控 + requires 声明序断言）+ `emit_ensures_checks`
  （fall-through 与每个早退 Return 两处漏斗，`result` 绑定 lower.rs
  `{owner}/contract-result/local` 伪 local，检查后恢复 frame）+
  `emit_contract_assert`（E0808 abort 消息对齐 legacy scope.rs 格式，BB 名以
  条件 NodeId 去重免计数器）；`collect_old_nodes`/`collect_old_block` 全 variant
  递归收集；`ResolvedFrame.old_snapshots` + `Old` arm 快照加载（无快照时保持
  擦除恒等语义）；eligibility 的 verify_contracts fail-closed gate 删除
  （条件表达式仍由 require_block Contract arm slice 检查，不支持则 per-function
  降级 legacy，不静默丢守卫）；消息中条件文本降级为 span 坐标（resolved IR 无
  surface 渲染器；VM 本身打印的是求值结果值，跨后端文本平等本就不存在）；
- **附带修复存量 L1（第二档测试暴露）**：legacy 早退 Return 六处 ensures 断言
  无 `result` 绑定——`block.rs` compile_block 值/None 臂 + 块表达式路径值/None
  臂 + `actors.rs` 方法体值/None 臂，任何引用 `result` 的 ensures 在这些路径
  直接编译失败（undefined variable 'result'）；抽 `compile_ensures_asserts`
  共享 helper（scope.rs，result alloca + 绑定 + 断言，对齐 func.rs emit_return
  语义）统一六处；
- **第二档验证**：探针矩阵 p1-p6 双端对等（requires 违反 / ensures 违反 /
  早退 Return 违反 / old()+result 通过 / 多合约函数，VM 与 native 逐项一致）；
  dual 回归 ×3（`dual_contract_verify_ensures_old_result_on_resolved` /
  `..._early_return_ensures_violation` / `..._multi_clause_pass`）；全量 5271
  lib 绿；门禁 check 无回退（fallback_rate 0.9609 不变——第二档只动
  verify 路径，默认模式分派第一档已定型）；

### 0.34.40 — resolved dispatch 度量门禁（AF-4 前置 1，纯增量基建）

> Phase G（0.34.39-45）第二个 sprint：进 0.1.5 前把 legacy 受管退役的度量地基钉死——
> fallback 率成为一等指标 + CI 禁静默回退。依据 ADR-005（legacy 受管退役）。

- **MIMI_STAT=1 结构化分派报告**：`DispatchStats`（`src/codegen/resolved/eligibility.rs`）
  收集 eligible/legacy 计数 + skip 原因直方图；`eligible_function_ids_with_stats`
  逐函数记录 skip 类别；`emit_dispatch_stats` 写 JSON 到
  `MIMI_STAT_OUT`（默认 `target/mimi-stat/<src>.json`）；
- **skip reason 规范化**：`normalize_skip_reason` 折叠含 `NodeId(`/`@external:`/
  `ResolvedTypeId(`/`ResolvedCall {` 等不稳定 Debug 输出的 reason 为稳定类别
  （unsupported expression/statement/pattern/type/callee）——避免源哈希跨会话
  漂移导致门禁误报 + JSON 膨胀；
- **基线语料入仓**：`devdocs/v0.34/golden/dispatch-baseline.json`（demos/+examples/+
  tests/real_world/ 全跑）——112 程序编译成功 / 16 跳过，202 eligible / 5425 总函数，
  聚合 fallback_rate=0.9628（符合 AF-4 草案“resolved 覆盖 = entry 顶层无泛型无
  合约标量函数”的现状描述）；
- **门禁脚本**：`scripts/dispatch_stat.py`（generate/check/report）。check 模式对比
  基线禁静默回退率上升（EPSILON=1e-9）；白名单登记制
  （`devdocs/v0.34/golden/dispatch-whitelist.json`，reason 必填，`_` 前缀说明 key
  过滤，同 ignored 测试纪律）；新程序首次纳入不判回退；TMPDIR 指向工作区适配
  sandbox 只读 /tmp；
- **门禁自检三项全过**：无回退→exit 0；篡改静默回退→exit 1（报 `demos/01_basics.mimi:
  0.5000 → 0.9767`）；白名单登记→`[whitelisted]` 放行 exit 0；
- **MIMI_VERBOSE info 行格式冻结**：既有 `info: resolved skip/…` 行格式不变
  （向后兼容承诺，本 sprint 仅新增 stats 收集不改 verbose 输出）；

### 0.34.39 — 架构裁决战役：四项 One-Way Door 升格正式 ADR（纯裁决，零实现）

> Phase G（0.34.39-45）启动 sprint：进 0.1.5（性能主线）前把架构形状钉死——裁决先行，
> 实现分档。依据 `devdocs/v0.34/architecture-freeze-draft.md`（四项深度评估 + 裁决草案
> + §8 版本战略重估）。

- **ADR-005 legacy 发射器受管退役（AF-4）**：推翻“长期保留”旧裁决——退役终态
  （legacy 不再编译函数体，三引擎函数体统一 Resolved IR）+ 四前置（度量门禁/
  假边界消灭/单态化/机制迁移，顺序不可颠倒）+ 回退纪律（回退>0.5% 或 SIGSEGV 即
  回滚）；实施 0.34.40-43 启动，终态 0.2 轨；
- **ADR-006 codegen 内存所有权账本（AF-1）**：编译期所有权账本（复用 CFG +
  ResourceAnalysis，与线性能力 exactly-once 同一套机器）——字面量非 owned 不入账本、
  builtin returns_owned 目录、per-turn 固定容量池分配策略优先评估（QP/C 参照）；
  实施 0.2 轨；
- **ADR-007 值表示 ABI 布局冻结（AF-2）**：冻结当前 handle/`{ptr,len}`/has_header
  布局 + Component IR abi_version 握手做 Two-Way Door；0.34.45 定稿文档 + 0.2 实施；
- **ADR-008 Verifier 引擎收敛（AF-3）**：resolved 主引擎 + VIR 降级 math: 通道 +
  缓存引擎隔离（键=(program_hash, engine, clause)，分歧 fail-closed 新诊断码）；
  实施 0.34.44；
- **版本战略**：0.minor=大版本（Zig 式，每 minor 边界即冻结点 + 允许 breaking +
  必附迁移注记）；AF-1/AF-4 前置 3/4 重分类 0.2 轨；pre-1.0/ → pre-0.1/ 更名随
  0.34.45 落地；草案标“已升格”归档，README §4 裁决表补行 ADR-005~008；

### 0.34.38 — 字符串参数守卫战役 + 2026-08-07 大项清扫 + R-2 确定性选择

> 依据台账 `devdocs/audit-unfixed-2026-08-05.md` §14（字符串参数守卫战役）+
> §12 结构性项。战役动机：LLVM 层 List 值以原始指针传递（List = ptr to `{i64,ptr}`），
> 与字符串指针在 emitter 内不可区分——宽松 builtin 参数不受 checker 约束导致
> `str_trim([1,2,3])` strlen 一个 List struct → 垃圾输出 / `into_pointer_value()`
> panic / abort(核心转储)。本次系统性消除该族。全量 lib 5263 → 5273 全绿。

- **string 家族编译期守卫（`7c5574ed` + `1fde0249`）**：表驱动
  `string_only_builtin_string_args` 按参数位守卫（str_repeat/str_char_at/str_parse_int/
  str_parse_float/string_to_int/str_to_c_str 位0、str_split/starts_with/ends_with/
  index_of/count_substring/regex_match/regex_find/regex_find_all 位0,1、str_replace/
  regex_replace 位0,1,2、str_join 位1）；`is_definitely_not_string` 保守判定未知
  类型放行防误拒；emitter 布局检查防 panic。**str_contains/starts_with/ends_with/
  regex_match 返回 i1(bool) 修 L1 分歧**（checker 推断 bool、VM 返回 bool、codegen
  此前 zext i64 打印 1 vs true）；
- **contains 多态接收者（`827a2764`）**：全局 `contains("hello","ell")` 曾
  SIGSEGV（compile_contains 把 string 当 List 指针 load_list_len）；string 干草堆
  重定向 str_contains + needle 守卫；compile_contains 返回 i1；resolved 字符串
  方法映射（`builtin.method.string.X` → str_X，消除 E0709）；
- **json/crypto 家族守卫（`b137e088`）**：sha256/base64_encode/base64_decode/
  from_json/json_is_valid/json_array_length/json_get_element（位0）、json_get_string/
  json_get_int/json_has_key（位0,1）；消除 List 参数 abort(exit 134)；
- **fs/env/path/exec 守卫（`0771be95` + `26ca5af5`）**：read_file/write_file/
  path_join/set_env 等 List 参数编译期拒绝；bool 谓词返回 i1 修 L1 分歧；
  exec_safe varargs 守卫——List 参数静默成为垃圾 argv 修复；
- **network + lexer 守卫（`0ebf8a04` + `c9fbad3b`）**：http_get/http_post/send/
  connect 字符串参数守卫；lexer 内建字符串参数守卫；
- **str_contains List/Set 干草堆路由（`c1e2300f` + `9e04bf94` + `cff7c640`）**：
  List 干草堆路由 compile_contains、Set 干草堆路由 mimi_set_contains、函数形式
  `contains(Set, x)` 同路由（消除 VM-only gap）；type_name(x) 修复 Located 解包 +
  返回规范 string struct；
- **§6-#65 variant 名子串错配（`508147db`）**：lookup_variant_name fallback 改
  精确后缀（`variant.Errors` 不再误配 Err）+ §4-#44 AST 重复条目删除 + enum
  variant type_name 登记；
- **§7-#81 fn pointer 返回 ABI 错配（`b6d1730d`）**：间接调用返回类型从
  var_types 恢复（closure_return_llvm_type 同款），f64 不再读 %rax 垃圾；
- **D-5 substring strict 越界（`05f354f7`）**：方法形路由新 builtin
  str_substring_strict（mimi_str_substring strict runtime），函数形保持 clamp；
  resolved 直连 ABI 桥修复（STRING_ABI_BUILTINS 跳过 runtime 直连走 emitter，
  E0722 消除）；
- **2026-08-07 大项清扫（`5e30cf8a`）**：§11-#37 Z3 命名空间残留清扫（call_var_key
  歧义拼接改 `#` 分隔 + `{p}.len`/`{p}.ne` 派生常量改点分隔，消跨调用别名）；
  R-1/V-11 嵌套函数遮蔽 lowering 歧义（shadowing_nested_function 镜像 checker
  声明序裸名注册 + codegen 遮蔽符号重定向 + 帧守卫，LLVM ERROR 消除，#[ignore]
  已解）；§4-#41 module/actor NodeId 碰撞（collect_item_decls 补 module path）；
  B-7 print_err auto-deref（builtin_eprintln 改走 io::print_display）；to_int/
  to_float 消息差异（静态已知聚合类型以 VM 对齐消息 `[E0800] cannot convert
  this type` 编译期拒绝）；测试基建：audit_fix_io 两条 VM input EOF 测试加
  IsTerminal 守卫（tty 环境读真实 stdin 永久阻塞曾冻结 audit_fix 运行 1.5h）；
- **R-2 构造器模式确定性（`0b909de6`）**：flow_variant 查找提取为
  `pick_matching_record_def`（字典序最小 variant id 确定性选择，注释钉死 R-2），
  消除 `HashMap::iter().find()` 非确定遍历 + 缺失 field 捏造；
  `r2_pick_matching_record_def_is_deterministic` 回归（50 次 fresh-HashMap 断言
  稳定 + 全名匹配 + 非 record/未知名不匹配）；

### 0.34.37 — 显示截断族修复：legacy 显示固定缓冲 → 精确尺寸组装

> §8-#96/D-4 固定缓冲显示截断族全闭（台账 `devdocs/audit-unfixed-2026-08-05.md`）。
> 实证复现：VM 404/305/606 字符 vs legacy 255/255/511（256B/512B/固定缓冲−1），静默数据丢失。
> 全量 lib 测试 5260 → 5263 全绿（+3 回归）。

- **显示四族固定缓冲 → sized_cat_parts 精确组装**：enum 128B snprintf（emit_enum_display）、
  record est-size snprintf（emit_record_display）、Result 256B snprintf（emit_result_to_string_typed）、
  Option 512B snprintf（emit_option_to_string）全部改为两遍 strlen 精确 malloc + memcpy 组装，
  统一 out_slot+merge 注册 + 臂内局部 display_marker 生命周期模式（共享前置 marker 会跨臂
  污染并释放 merge 注册的 wrap → UAF）；长 payload 双端字节一致（404/305/606 不再截断）；
- **预存崩溃 bug 顺手闭合**：`Result<string,string>` 的 Err(string) 槽 legacy 存裸数据指针
  （运行时字符串，如 str_repeat）或 {ptr,len} 结构指针（字面量）——结构解码对数据指针
  valence 把 payload 字节当指针 strlen → SIGSEGV（0.34.36 HEAD 同崩，非本轮引入）。
  str_bb err 分支改双通道：mincore 探测 field0 可读性（新 runtime helper
  `mimi_runtime_ptr_readable`）区分两条构造路径，字面量与运行时字符串均正确显示；
- **D-4 复核闭合（台账过时）**：List 显示族已无固定缓冲（runtime 动态分配 +
  sized_cat_parts），List<string> 双 500 字符探针 1005 字节完整；
- **回归测试**：audit_896 ×3（Result/Option 长 payload 双端全量字节断言 + enum/record
  长字段 + 短路径臂精确字节）；

### 0.34.36 — 审计台账收尾战役：audit-unfixed-2026-08-05 收尾包 A–F 全闭

> 依据 `devdocs/audit-unfixed-2026-08-05.md`（单一事实源）。纪律：冻结期不加新语法，
> 只让报错说真话（fail-closed / fail-loud），每项修复带回归测试。全量 lib 测试
> 5228 → 5260 全绿（+7 登记 ignored 不变）。

- **收尾包 B（checker/codegen/VM）**：K-4 数值强制双门禁复核闭合（防回归测试）；
  K-5 resolved Drop 对 Capability place 从静默 no-op 改 fail-closed（E0830）；
  H-9 match 落空分支发射 `Op::NonExhaustiveMatch`（E0805，与 codegen 对齐）；
- **收尾包 A/C**：§2-#19 bound-generic trait 方法分发诚实拒绝（新诊断码 E0437，
  单态化延 1.x，Clone 特化保留）；§11-#46/47/48/50 verifier 四项——i64 算术溢出
  definedness 义务、f64 let 绑定编码、match 无约束 fallback、Unknown→Timeout 分流；
- **收尾包 E（诊断卫生/CLI）**：§13-#67 `--verify-contracts` 真接线 + `--allocator`
  非 system 拒绝 + `--verify-ffi` VM 未实现警告披露；X-5 loader is_dep 子串判定改
  结构化（stdlib_dir 前缀 + [".mimi","deps"] 组件窗口）；E0830 入 Diagnostic.code；
  E0231→E0438 拆分、E0422→W0402 降级（retired 码不复用）；§1-#10 f-string 插值
  双层引号感知扫描；
- **收尾包 D（runtime 安全）**：§10-#27 ReDoS 时间预算——`REGEX_MAX_WORK`（1M 步/次）
  贯穿两个回溯引擎全 5 入口，耗尽失败 + 每进程一次警告；§10-#31 product 序列化器
  4 处裸 `from_raw_parts` + list 头解引用改 mincore 探针（不可读句柄零值 + 警告，
  不再 SIGSEGV）；§10-#35 `mimi_map_destroy` 回收 map 自持 value 缓冲（Pack/
  ListOfPacks 形状登记，balance 归零可观测）；§10-#22 mailbox 动态化裁决延 1.x
  （dispatch ABI 契约变更）；
- **收尾包 F（语义/工具链）**：V-7 pow 类型契约裁决——checker 按实参推 pow
  （int×int→i64），codegen 删 sitofp 强制转换，pow(2,60) 双端精确渲染；
  `__mimi_pow_i64` exp>u32::MAX 双端回归补齐；stress 套件改双向契约（守卫内必过 +
  越界必响亮报 recursion limit）；nextest 幽灵键 `test-timeout` 清除（被 0.9.140
  静默忽略，硬上限由 slow-timeout period×terminate-after 承载）；stdlib JSON 与
  serde 语义对齐（1e999 f64 溢出拒绝，双端等价）。

### 0.34.35 — FFI 审计闭环：repr(C) 导出 SysV ABI 修复 + fn 字段调用 L1 修复

> 依据 2026-08-05 Jupitune dogfood 外部评估审计（`devdocs/v0.34/dogfood-jupitune-eval-0.34.34.md`，
> 17 条复核 14 成立 + 3 个审计新发现）。本 sprint 处置 L1/L2/L3 级阻断 bug；
> 特性缺口（f32/指针/数据符号/vtable）登记 0.2 不进本版。
> **0.34.35b/c 收尾（2026-08-07 随 0.34.36 战役落地）**：M-001 extern 符号
> 真实命名 + M-006 dlopen ABI 测试族 + M-011③ 直接调用诚实拒绝 + N-3 警告
> 洪水 + M-003/016/017 文档统一，见下。

- **N-2｜fn 字段调用 codegen 静默误编译修复（L1）**：裸函数引用存入 closure 型
  record 字段（`type VTable { add: func(...) }`）时，codegen 把 8 字节 fn 指针存进
  16 字节 `{fn_ptr, env_ptr}` 槽、env 半初始化，调用端把垃圾 env 当首参注入——VM
  结果对、codegen 静默错值（`f(1,2)` 返回 255）。修复（`codegen/expr/record.rs`）：
  静态具名 callee 走 `{closure_wrapper, null}`；运行时 fn 指针（如变量持有）走
  **签名键 trampoline**（callee 坐 env 槽、trampoline 以声明签名间接调用，无 env
  注入）。真 closure（lambda）不受影响。新增 5 个 dual 对齐测试（含捕获 lambda 回归）；
- **M-010/N-1｜repr(C) 导出 SysV ABI 修复（L3 内存安全）**：`extern "C"` 导出函数
  的 repr(C) struct 按值传递此前违反 SysV——`is_simple_reprc_record` 仅 ≤2 全 i32
  走寄存器、其余一律当指针，`{i64}`/`{i64,i64}` 参数把寄存器值当指针解引用 →
  C 侧 dlopen 调用 SIGSEGV，≥24B 返回垃圾，debug 编译器对部分形状直接段错误。
  根因双层（`codegen/func/export.rs`）：
  ① ABI 侧实现 **SysV eightbyte 分类/coercion**——≤16B 按 8 字节分类（INTEGER→i64、
  SSE→double）用 coerce 寄存类型穿越边界（LLVM 原生 struct 参数不做 SysV 合并，
  实测 `{i32,i32}` 被拆到 edi+esi 而 C 调用方打包进 rdi）；>16B 参数加 `byval`
  （SysV 栈传递，裸 ptr 读 rdi 是垃圾）、>16B 返回加 `sret` 隐藏缓冲。
  ② IR 侧把 `const_named_struct` 喂运行时 SSA 的用法全部改 `insertvalue` 链——
  前者产出"伪常量"IR，独立 `opt` 报 `invalid use of function-local name`，
  LLVM-18 优化器在 LazyCallGraph/InstCombine 处段错误（即 debug 编译器崩溃根因）。
  dlopen 探针 5 参数形状 + 2 返回形状 + 混合 SSE 全通过；顺带消除旧堆指针返回的
  per-call malloc 泄漏；
- **测试纪律：nextest 每测试硬超时**（`.config/nextest.toml`）：每测试独立进程 +
  60s 硬上限（slow-timeout 30s 标黄两次即杀）+ leak 检测。动机：全量 `cargo test` 遇
  死锁/死循环会无声挂起只能肉眼盯进度；独立进程使段错误/死锁被单独报失败而非拖死
  全场。default 60s / ci profile 120s（对齐 2 vCPU runner）。全量 nextest 复跑绿：
  4623 passed / 0 failed / 7 skipped。（0.34.36 补记：原 `test-timeout` 键为
  nextest 不识别的幽灵键，硬上限实际由 slow-timeout 承载，已清理。）
- **0.34.35b｜M-001 extern 符号真实命名（L1 分裂）**：extern 符号默认直接使用
  声明名（`func strlen` 链接 C 库的 `strlen`，与 VM 侧 `lib.get(name)` 一致），
  `__mimi_extern_` 前缀机制移除；内部测试桩经显式完整符号名保留
  （`__mimi_extern_test_*`）。LLVM 侧先查模块复用同名兼容符号（签名不兼容
  → fail-loud 诚实拒绝，不 mangle 成连不上的名字）；`demos/14_ffi.mimi`
  端到端链接真实 libc 可跑；
- **0.34.35b｜M-006 dlopen ABI 测试族**：`--shared` + C dlopen 往返探针常规化——
  编译 mimi 共享库 → C 探针 dlopen/dlsym 调用导出函数 → 校验返回值（
  `build_shared.rs` `dlopen_roundtrip` helper，8B/16B/24B/混合 SSE 形状矩阵 +
  f32 缺位负测试）；
- **0.34.35b｜M-011③ fn 字段直接调用诚实拒绝**：`vt.add(1, 2)` 字段直接调用
  此前 E0207 吞参静默错值；现 E0223 明确诊断
  （"field 'add' of 'VTable' is a function value and cannot be invoked directly
  on the record"）+ help 指导（bind 后调用），audit_fix_checker 回归绿；
- **0.34.35c｜N-3 component-ir 警告洪水闭合**：四个 trap 运行时函数
  （mimi_trap_overflow/div_by_zero/div_overflow/float_not_finite）登记 Component
  IR 注册表（component/gen.rs），构建不再刷屏
  `get_runtime_fn("mimi_trap_*") not in Component IR registry`；
- **0.34.35c｜M-003/016/017 FFI 文档矛盾统一**：ffi-guide §4 i32 宽度改正
  （int32_t，与 ffi-type-mapping 一致）；`mimi run` 非零退出码回显
  （`-> <exit_code>`）行为文档化（readme/07-cli.md）；spec §7.4 ffi
  slice/buffer 标注"未实现（0.2 评估）"；devdocs ffi-1.0-surface-eval §5
  已就绪清单更正注记（M-010/N-1 实证为假）；

### 0.34.34 — i32 算术语义钉死：SD-7 trap 对等 + O1 毒值修复（L1 双后端等价闭环）

> 用户报告 i32 标注算术 L1 分歧（VM 以 i64 宽度静默 wrap / codegen E0802 trap），
> 顺藤摸出两处 O1 毒值。本 sprint 将 i32 运算语义在双后端钉到同一裁决上：
> **算术溢出 = trap（SD-7）；收窄绑定 = trap；显式 cast = wrap；界外移位 = 硬件掩码**。

- **VM i32 宽度保真**：bytecode compiler 新增 `VarType::Int32`（let 声明的标量
  标注**优先于**类型推断），新操作码 `CheckI32` / `CheckI32DivRem` / `WrapI32` /
  `MaskShiftAmt`；binop/let/assign/neg 全部带 i32 守卫；`as i32` cast 走 trunc+sext
  wrap 语义。trap 消息与 codegen 对齐（`integer division overflow (MIN / -1)`、
  `integer overflow in power`）。此前 VM 把 i32 值当 i64 全程运算，`i32::MAX + 1`
  静默返回 2147483648——现已按 E0802 trap；
- **双后端 SD-7 trap 对等**：i32 add/sub/mul、div/mod（MIN ÷ -1）、一元负号
  （`-i32::MIN`）在 VM 与 codegen 上均 E0802 trap、消息一致；
- **收窄绑定守卫（narrowing bind/assign trap）**：向 `i32` 注解槽位绑定/赋值一个
  越界宽值是 E0802 overflow，不再静默 trunc。五个编译点全覆盖——legacy 顶层函数体
  `compile_func_body`、嵌套块 `compile_block` / 取值块 `compile_block_last_val`、
  legacy 赋值 `assign_to_var`、resolved emitter `bind_pattern` + `Assign`
  （NumericNarrowChecked before-trunc 守卫）。配套根因修复：`Type::Located` 包装
  导致注解直接匹配 `Type::Name` **永远不命中**——新增 `annotated_type_name`
  pierce helper（此 bug 意味着既有一切按注解名的 legacy 守卫均有同款盲区）；
  显式 `as` cast 保持 wrap（不改语义）；
- **O1 移位毒值修复（SD-7 附带裁决）**：LLVM 未掩码移位为 poison——O0 下硬件指令
  自行掩码看似无害，O1 常量折叠 `1 << 65` → poison 毒值泄漏。裁决：**界外移位保持
  O0 可观察的硬件掩码语义**（`1<<65`→2、`1<<-1`→i64::MIN），在全部优化级别确定一致：
  codegen 移位前显式 AND 掩码（宽度-1），VM `Shl`/`Shr` 从 trap 改为 mod-64 掩码；
  `v1_2_core_edge` 两个旧 trap 预期测试改写为掩码语义断言；
- **O1 physreg crash 修复**：multi-target transition 函数在已终结块后被
  `emit_implicit_return` 追加游离 `ret`（无效 IR → `LLVM ERROR: Cannot emit
  physreg copy instruction`）——追加前检查 `block_has_terminator()`；
- **O1 优化改为默认**：`MIMI_OPT` 语义反转为 opt-out（`MIMI_OPT=0/false` 关闭，
  未设置或 1/true 保持 O1）。0.31.21 已修复 O1 codegen 两 bug（try_expr i32-vs-i1、
  extern wrapper 名冲突），本 sprint 4618 全量 + golden + CLI 冒烟矩阵在 O1 默认
  下全绿，满足放行基线；
- **诊断输出契约**：新增 `docs/diagnostics.md`（normative 文法：单行致密
  `SEVERITY[CODE] LOCATION MESSAGE | field:...`，机器/AI 优先，caret/gutter 装饰
  由坐标区间无损替代）；runtime `mimi_trap_overflow` / `mimi_trap_float_not_finite`
  的 Hint 行并入单行 `| hint:` 字段；`docs/error-codes.md` 登记契约引用；
- **测试**：`dual_backend.rs` 新增 12 个 i32/i64/cast 对等测试（trap 对等 +
  shl 掩码 + pow wrap + const-fold 收窄 + cast wrap）；golden IR 4 件重生成
  （recursive_fib/pipeline/mutual_recursion/try_operator——收窄守卫的 icmp/br
  插入，合法程序守卫恒通过，无行为变化）。全量 4618 passed / 0 failed / 7 ignored，
  CLI 冒烟矩阵（trap 对 / shl / cast / 收窄 / fib 不误杀）在 O0（MIMI_OPT=0）与
  O1 默认下全部一致。

### 0.34.33 — 审计收尾：文档同步残留闭环 + 门禁加深（0.1.4 深度审计修复）

> 依据 2026-08-04 0.1.4 全量深度审计（对照 pre-1.0/05 RC 门禁 + v0.34 验收标准，
> 全门禁实测：fmt/clippy/check_unsafe_safety/4598 lib + 13 real_world + 28 cli 全绿）。
> 审计结论：工程侧与代码实现全部达标；0.34.28 同步战役残留 3 处 P0 + 多处 P1/P2，
> 本 sprint 全部闭环，不留尾巴。

- **外围文档族普查闭环（第二轮）**：
  - **README.md / README.zh.md**：Hello Flow 主示例 `do { return }` 迁移为裸
    `return`（0.34.27 删除语法不再教给用户）；特性表 multi-target 与 View/Mutate
    由 📋 升 ✅（均已落地）；Progressive mode 注记"真 lowering post-1.0"纠正为
    已实现的语义脱糖（spec §3.13）；0.1.4-dev 版本行重写（trivia 化误称为 0.1.4
    成果 → 明示登记 0.1.5，补齐 do 删除/软关键字/ieee_float/数值强制等实况）；
  - **docs/spec/transition-turn.md**（normative profile）：Terminal outcome 分类
    `Become`/`Stay` → 统一 `Commit(target, payload)`（ADR-001 术语对齐）；Turn 配置
    移除已废止的 `transaction_log`（修正案条款 3 WAL）；multi-target 条款升 stable 表述；
  - **docs/ast-appendix.md**：快照版本刷新（v0.30.0 → 0.1.4-dev）；Stmt 表 `Do`/
    `Delegate` 改 absent（variant 已删）、`Math` 改 verifier 通道描述（0.34.28 裁决）、
    补 `IfLet`；Expr variant 行纠正（31→34，删已不存在的 `Range`）；§2.1 Progressive
    maturity partial→complete（0.34.28 实测）、§2.2 N×M 自动补全 partial→repealed
    （0.34.18b 稀疏图 + E0211）；表格措辞遵守该文件"禁状态词汇"既有门禁；
  - **readme/ 教程族**：00-index/01-syntax/llmprompt 加 MimiSpec 时代快照状态 banner；
    01-syntax §2 与 llmprompt §2.4 关键字表**整体重写为冻结实况**——与
    keywords.rs:92-177 程序化比对 **80/80 精确零差**（删 steps/ui/binds/on 等已失效词，
    补 flow/state/transition/session/dual/end 等全部现行词）；llmprompt 版本行
    （v0.7.0 时代）加诚实化注记；
  - **gramma/mimi-grammar-v1.0.0-rc.md**：加"不再权威"状态 banner，纠正过时的
    MimiSpec 保留字声明（flow 已是 Mimi 关键字）与权威性措辞；
  - **门禁再加深**：`check_language_docs.py` 语义新鲜度扫描面扩展至 README/README.zh/
    docs/spec/*.md/docs/ast-appendix.md；REMOVED_MARKERS 补 `removal`。
- **核心文档族闭环（第一轮）**：P0×3（spec §6.12 math/multi-target 清单 +
  requirements FLOW-TURN-001）、pre-1.0 九份源头、support.toml 三条 evidence、
  syntax-reference 真再生成、await 撤销回写、golden 记账、测试卫生、根部垃圾清理。
- **CI/CD 流水线深度完善**：
  - **致命缺陷×2 修复**：release.yml 环境变量 `LLVM_SYS_180_PREFIX` → `181`
    （llvm-sys 181.3.0，旧名导致发布构建找不到 LLVM）；tag 触发器 `mimi-v*` →
    纯 semver `[0-9]+.[0-9]+.[0-9]+`（AGENTS §15.1 已弃用 mimi-v* 前缀，旧触发器
    导致 0.1.x tag 永不出包），mimi-v* 保留兼容匹配；
  - **新增 composite action `.github/actions/setup-mimi/`**：LLVM 18（apt-key 弃用 →
    keyring）+ wrapper + z3/ffi/binutils + toolchain + cache 收敛为单一事实源，
    lint/test/release 三处 ~40 行复制安装脚本退役（此前已漂移：release 缺 binutils）；
  - **门禁补齐**：`check_language_docs.py`（Spec 同步闸 + 0.34.33 语义新鲜度探针）
    进入 lint job；`cargo test -- --ignored` 收编为 advisory 门禁（continue-on-error，
    实测无 ASan/Valgrind 工具链时 6/7 失败——allow-fail 是必需而非防御）；
  - **稳定性防护**：全量测试步骤 `ulimit -v 20000000`（超限 kill 不冻 runner；
    实测全量峰值 RSS ~307MB，§3 的 12GB 为旧时代数据）；Z3 验证套件单线程隔离步骤；
    全部 job 补 timeout-minutes；顶层 concurrency 取消同分支旧运行；
  - **发布加固**：release 前 `mimi check` 冒烟、产物补 sha256sum、发布说明从
    CHANGELOG 提取当前版本小节（缺失回退全文，旧配置整文件灌入每个 release body）；
  - **Makefile `ci-check` 假门禁修复**：clippy/fmt 的 `2>/dev/null || true` 吞失败
    移除，补 language-docs/unsafe 门禁，与 CI 对齐；
  - AGENTS §3 内存警告补实测数据注记、§15.3 门禁清单重写为新流水线实况。
- **门禁结果**：fmt/clippy/unsafe gate/check_language_docs 全绿；Rust 代码本轮零改动，
  测试状态与 0.34.32 RC 复核一致（4598 lib + 13 real_world + 28 cli）。

- **P0 闭环（spec 同步闸违规项）**：
  - spec §6.12：`math` 移出 Removed 清单（§5.6/§6.8 已标 `[stable]`，清单漏改）；
    multi-target 与 math 升入 Stable 清单（multi-target 移出 Experimental 后未并入）；
  - `docs/language-requirements.toml` FLOW-TURN-001：become/stay 现行时态描述
    改为 `return State {}` 唯一终止符（ADR-001 0.34.11）——同步战役漏掉的规范索引文件；
  - spec 头部差异登记表四条全部闭环（`|>` 条目补 ✅）。
- **P1 闭环**：pre-1.0 源头九份文档残留（04 multi-target experimental 位置性矛盾×2、
  04/03 math 移除正文 superseded 注、04 Removed 清单 ADR-003 保留项、AGENTS §0 注记 +
  坐标漂移 850→883 修正——本地文档族）；support.toml 三条 evidence 刷新
  （FLOW-PROGRESSIVE-001 implicit main 实测纠正、SYNTAX-REMOVED-001 刷新至 0.34.27、
  FLOW-MULTI-001 partial 维度澄清）；`docs/syntax-reference.md` 从 golden 真正重新生成
  （关键字 81→80、软关键字枚举补 and/or/not、§12.2 残留与自指差异表删除，
  正文与 golden 逐字节一致）；golden §1.3 关键字 box 算术修正（补 and/or/not、删误列 i32）。
- **P2 闭环**：裁决回写断链——await 删除裁决撤销标注回写（feature-decision-0.34.23、
  golden §10 0.34.23 行、v0.34 README Phase F 行）；golden §10 记账修正
  （0.34.1/0.34.18 盖章、0.34.9 计数 30→31、0.34.19 soft 计数 1→3）；
  dx-backlog 补登记 #13（runs_flow E0221 三层集成）；测试卫生
  （`multi_target_incompatible_payload_layout_rejected` 改名 `_accepted_adr002` 名实相符 +
  `flow_turn_become_multi_target` 陈旧 E0226 TODO 删除并补 checker E0420 fail-closed 护栏）；
  根部垃圾清理（13 个零字节 test_physreg_*.o + 零引用 scratch_lexer.mimi）；
  0.1.5 zonk 迁移脚手架（`zonk_or_unknown`，dx-backlog #1）收纳提交。
- **门禁加深（防同类回归）**：`scripts/check_language_docs.py` 新增语义新鲜度检查——
  become/stay/do 现行时态引用、multi-target experimental 降级、math `[removed]` 标注
  行级探针（同行裁决标记豁免历史引用）+ spec §6.12 结构引脚 + 关键字计数漂移引脚 +
  FLOW-MULTI-001 target=stable 引脚。实测 5/5 历史违规复现触发、6/6 合法引用零误报。
- **门禁结果**：fmt/clippy `-D warnings`/check_unsafe_safety（0 violations）/
  check_language_docs（31/31 + 新鲜度）全绿；4598 lib（0 failed / 7 ignored）+
  13 real_world + 28 cli 全绿。0.1.4 保持 `0.1.4-dev`（tag 按用户裁决暂缓）。

### 0.34.32 — 0.1.4 RC 复核 + 工具门禁（Phase F 收尾）

- **unsafe SAFETY gate 清零（61 → 0）**，CI lint 门禁恢复全绿：
  - `scripts/check_unsafe_safety.py` 增强：
    - **raw string 剥离**：mimi 测试源码以 `r#"..."#` 嵌入 `unsafe { }` 语言关键字块（`unsafe` 是 Mimi 语言特性），此前被误计为 Rust unsafe（6 处误检）；现在先整文件剥离 raw string 再行扫描。
    - **行号保留**：raw string 替换保留换行数，`--list` 输出行号与源文件一致（此前偏小 ~200 行）。
    - **rustfmt 布局兼容**：`unsafe { // SAFETY: ... }` 被 rustfmt 移到块内首行时同样识别。
  - 为 **61 处真实 unsafe 补 `// SAFETY:` 注释**，含 0.33 bytecode 迁移存量：
    - `bytecode/builtins/net.rs` 26 处：libc socket/getsockopt/getaddrinfo/freeaddrinfo/bind/listen/accept/send/recv/connect/dup2/close/`__errno_location` 标准调用，参数经 builtin 校验；
    - `interp/ffi_runtime.rs`：runner 裸指针（`Option<*mut dyn FfiClosureRunner>`）同步调用期有效性、CStr 借用、`Box::into_raw/from_raw` 配对、`call_ffi_raw_struct` 低层入口；
    - `bytecode/builtins/misc.rs`：close_fd、C 侧字符串 CStr 读取 + `libc::free` 配对；
    - `ffi/callback.rs`：回调参数槽 C 字符串释放（IP-C3 allocator match）、`FFI_CALLBACK_CTX` runner 指针；
    - `codegen`：inkwell `build_gep`/`BasicBlock::delete`（LLVM 构建 API）、`errno.rs` `strerror_r`（线程安全 XPG 变体）；
    - `tests/` + `main/run.rs`：测试 harness FFI 回调、`libc::flock` 文件锁、`libc::signal` SIGINT 安装。
  - 门禁结果：**0 non-runtime unsafe without SAFETY**（baseline 37），此后新增 unsafe 必须带 SAFETY。
- **RC 复核**：全量 4598 lib（0 failed / 7 ignored）+ 13 real_world + 28 cli 全绿；clippy `-D warnings` + `fmt --check` 干净。0.1.4 保持 `0.1.4-dev`（用户裁决不 RELEASE）。

### 0.34.31 — 内存模型切片评估（Phase F 之五，诊断闭环）

- **valgrind 实证三类泄漏（codegen 路径）统一根因** = **codegen C-ABI 无托管内存所有权模型**：
  - **to_json 标量缓冲** 512 B/次（`builtins/json.rs` Float/Bool/Int 三路径 `malloc_or_abort(512)` 裸返回，注释 `returned value owns the allocation`，消费端零释放）；
  - **字符串拼接** 51 B/次（`acc = acc + "x"` 每次 concat 分配新缓冲，旧 acc heap 指针覆盖丢弃，独立于 to_json 复现）；
  - **List 容器 box** 24 B/元素（`List<(i32,i32)>` push 时 data buffer 生长 + 元素装箱，List 无析构）。
  - 运行时**已有** `mimi_string_free`（mod.rs:923）等释放原语，但 **codegen 零调用点**——"有 free 设施、无注入策略"。bytecode VM 无泄漏（Rust 所有权），仅 codegen 路径。
- **裁决**：完整修复需方案 A（owned 字符串标记 `{ptr,len,flags}` + 消费端 free 注入点清单：重赋值/concat/println/to_json/FFI 边界）与方案 B（容器析构 glue），跨 checker+codegen+FFI 三端，属 **1.x 内存模型主线**（详细设计见 devdocs/v0.34/memory-model-0.34.31.md §3）。
- **不在 0.1.4 做**：Phase F 是语法冻结 + 语言自洽性战役；任何局部 free 注入都会在字面量（`{ptr,len}` 无法区分 `.rodata` vs 堆）与共享字符串场景触发 UB（L1 双后端大面积回归）。泄漏为**可量化固定量**（非无限增长），不构成 RC 阻断。
- 复现资产：`/tmp/leak_{json,concat,tuples}.mimi` + valgrind（评估文档 §5）。

### 0.34.30 — codegen 缺口补齐（Phase F 之四）

- **项一：LEGACY_CODEGEN 嵌套 list 索引测试转正**。harness 重构：从 `compile_and_run_with_config` 抽出 `link_and_run_module`（legacy/checked 两 harness 共享 object/link/run 逻辑）；新增 `checked_codegen_compile_and_run`（check → `compile_checked` → 链接运行，精确复刻 `mimi build` CLI 管线）。`tricky_nested_loop_list`（v1_4_tricky_interaction.rs）un-ignore 并改走 checked harness——此前被 `#[ignore = "LEGACY_CODEGEN"]` 掩盖（legacy `compile_file` harness 跳过类型检查、嵌套 list 循环索引误编译；CLI 全管线本就正确），现由 resolved dispatch 路径直接门禁。v1_4 31 全绿 0 ignore。
- **项二：trait-impl 方法 resolved emitter legacy 回退消除（dx-backlog #11）**。实测根因：含**非 i64 ok 载荷** Result（如 `Result<f64, string>`）的 trait-impl 方法函数体 resolved 编译失败——legacy `compile_err_constructor` 非 list 路径把 ok-pad 硬编码为 i64（产 `{bool,i64,i64}`），而 `Ok(1.5)` 产 `{bool,double,i64}` → if/else 分支合并时 `numeric_convert` 拒绝 struct→struct → 整个 impl 方法落 legacy → 所有调用方（ProtocolMethod callee）连锁落 legacy（dx#11 原判 "callee undeclared" 实为连锁表象）。
  - 新增 `emit_resolved_optional_ctor`：`Some/None/Ok/Err` 按目标类型 lower 布局构造（`{bool, ok_llvm, i64_err}`），merge/return 类型恒一致；
  - 新增 `resolved_err_to_handle`：err 载荷 → i64 handle（int widen/truncate、ptrtoint、string heap-pack `{ptr,len}`、enum tag），镜像 legacy B4 语义，`?` 算子的 inttoptr+GEP 重建 ABI 不变。
  - CLI 实测 `impl FloatGetter for string { func get(...) -> Result<f64, string> }`：`resolved emitter compiled 2/2 function(s), 0 fell back to legacy`（此前 1/2 fallback）；输出 `Ok(1.5)`/`Err(missing)` 双后端一致。
- **测试**：4598 lib / 0 failed / 7 ignored（6 工具门禁 + 1 Z3 专项，既有不变）；dual_ 830 + v1_4 31 + codegen_e2e 204 全绿；clippy + fmt 干净。

### 0.34.29 — actor 收敛（裁决重估 + SD-5 文档化，Phase F 之三）

- **await 裁决纠正（重要）**：0.34.23 U6 评估 "await 无意义 / interp no-op / codegen 无路径" 基于**过时证据**。实测：codegen `compile_await_expr`（expr/call/async.rs:327）是**真实 async**（mimi_executor_run + mimi_await_future spin-wait + future+8 结果加载），runtime/future.rs 提供 pthread future，dual_backend.rs "Both interpreter and codegen use real spawn/await with pthread" + `dual_actor_await_get`（1000 call no deadlock）+ real_world concurrency_spawn_await 资产。**await 是完整并发能力，保留**——0.34.23 "删除 await 语法" 裁决基于含错证据，正式撤销。正确语义：actor 场景 `await`（非 Future）由 E0245 `infer_await` 拒绝（helpers.rs:291，即裁决执行）；spawn future 的 await 保留。
- **runs_flow E0221**：`a.inc()` 误报修复经分析需**三层集成**（① infer method 注册 ② checker 注册 `zonked_function_types` ③ resolved `function:{Actor}::{method}` callable identity + lower_method_call）→ 登记 0.1.5。bytecode 已完整支持 runs_flow dispatch（run_source 42）；测试 `await a.inc()` → `a.inc()`（actor 调用同步，去 await）。
- **SD-5 mut 字段文档化 ✅**：spec §3.8 新增 `mut` Field Semantics——mut 是声明性标记（并发隔离提示）非写强制；1.0 不引入写强制（Flow 状态机平替 borrow checker，状态转移是唯一状态通道）；`runs Flow` actor 仍拒 mut 业务字段（E0402）。
- 相关源码未提交（infer/resolved 三层方案登记 0.1.5，不在本次落地半成品）。

### 0.34.28 — 裁决文档同步战役（Phase F 之二，sprint 规划 `sprint-0.34.28-doc-sync.md`）

- **文档族全链路对齐四组裁决**（do 已删 0.34.27 / math stable / multi-target stable / become-stay 已删 0.34.11）：
  - **spec** §5.6+§6.8 `math` 由 `[removed]` 修正为 `[stable]`（verifier 通道，Stmt::Math 由 vir.rs:495/878 消费）；§3.7 multi-target 由 `[experimental]` 升 `[stable]`（0.34.15-16 tagged-union ABI）且废止 ":369 not part of the minimum 1.0 RC stable core" 过时句；§6.12 Experimental 清单移除 multi-target；:143 多目标 tag 保留声明升 stable；:1565 RC 阻断条款改稳定表述；§6.12 `do` 加 `(0.34.27 已删除)`。
  - **pre-1.0 九份源头**（spec 的 source）12 处改写：00:83、01:45（Silent Stay→Silent transition 语义更名）、01:84-85（become/stay→`return Target {}`）、02:180、04:178 + §14 Removed 清单（do 加版本 + math 归 stable）、04:165/186、05:99/183/275/284（multi-target experimental→stable、terminal path 四类→三类）。
  - **support.toml** FLOW-MULTI-001 evidence 重写（tagged-union ABI 三后端门禁）+ resolved_ir/interp/codegen 维度如实化（unsupported/partial→complete）；become/stay evidence 术语 → `return S{}`。
  - **AGENTS.md** §13.1 0.1.4 里程碑范围扩至 0.34.32，补 Phase F（do 删除 81→80 达成 ≤80 + 文档同步）。
- **spec §3.13 implicit main 状态登记（实测推翻规划误判）**：`sprint-0.34.28` 规划引用 bytecode/compiler.rs:654 "no main function found" 判定"未实现"——实测 `mimi run` 输出正常，progressive.rs `apply_progressive_typestate`（v0.29.22）注入 `flow Main { state Single }`、main 体进 run transition，:654 仅为无 main 时的 entry 兜底。spec 改为"已实现"登记，非 not-yet-implemented。
- **验收**：check_language_docs.py 通过（31 requirements / 31 support）；`grep become|stay` 仅剩带版本的合法历史引用；"not part of the minimum 1.0 RC stable core" 归零。

### 0.34.27 — `do` wrapper 删除（语言自洽性，sprint 规划 `sprint-0.34.27-do-removal.md`）

- **AST**：删除 `Stmt::Do(Block)`（ast.rs）——`do { X }` ≡ 裸 block `{ X }`（ir/lower.rs 同路径 Lexical Scope + 裸 block 是表达式），无表达力损失。
- **关键字**：`do` 移出关键字表（keywords.rs `"do" => TokenKind::Do` 删除）→ 映射 **81→80，达成 0.1.4 ≤80 硬关键字目标**（golden §1.4）。`do` 现 tokenize 为普通 Ident。
- **解析/**：parse_stmt.rs `do { }` 语句分支删除；token.rs `TokenKind::Do` variant 删除；parser/helpers、pattern、parse_expr 的 TokenKind::Do 分类 arm 清理。
- **消费点清扫**（~28 点/15 文件）：cfg/ir lowering、resolved（kind_name stmt.do→block）、checker（borrow/func/check_stmt）、lint、loader/flow、codegen（block/func + **compile.rs transition do-unwrap 逻辑简化为 `t.body` 直接取块**——该 unwrap 代码本身印证了 `{ do { X } }` ≡ `{ X }`）、bytecode（Do arm 与 Block arm 逐字节一致，删除零风险）、verifier（Do arm 删除，顶层 Return/Expr/Let/If arm 天然接管）。
- **语料迁移**：24 real_world（84 处）+ flow_features（203 处）+ resolved/cfg/ir/audit/actors/codegen_e2e/diagnostic 测试全部展开。`{ do { X } }` → `{ X }`（transition body 扁平化）；普通函数内 do 块 → 裸块语句 `{ X }`。codegen 的 transition 返回值依赖实现体扁平，嵌套 block 会 SIGSEGV——已全部消除。
- **负测试**：`flow_do_keyword_rejected`（单行/多行 transition + func 三场景）——`do { }` 现被 checker 以「未定义类型 do 的结构体构造」拒绝（do 是普通 Ident）。
- **文档同步**：spec §6.7 实施注记 + :518 示例改裸 return；syntax-reference.golden + syntax-reference.md（EBNF 删 do 行、关键字表删 do、差异表 81→80）；golden-document §1.4 达成 ≤80 标记 + §1.3/§10 Phase F。
- **测试**：4597 lib / 13 real_world / 28 cli / 8 ignored（不变）；clippy + fmt 干净。

### 0.34.1–0.34.23（Phase A–E：语法冻结 + 语义裁决 + Flow 补完 + 1.0 准备）

> 0.34.24 之前的 0.1.4 主体 sprint。逐 sprint 权威记录在
> `devdocs/v0.34/golden-document.md` §10（Phase A–E），此处为发布用合并条目。

**Phase A（0.34.1-5）语法冻结前置**：
- 僵尸语法删除（delegate / @transactional / metadata_shadow / `|>` 分隔符）+ CI 负测试；
- 死关键字清理（subflow/steps/consume → Ident）+ `and`/`or`/`not` 软关键字化 +
  `Expr::Range`/`BinOp::Assign` 变体删除；DEAD 代码清理（pinned 路径 ~400 行）；
- `if let` 补实现 + `for (k, v)` 解构 + f-string BUG-5 修复；
- ADR-004 实施（`'a` 生命周期标注删除）+ SD-3 修正（`#[abi(errno)]` → `#[errno]`）+
  白皮书废止标注 + AGENTS §13.20 修正；
- `docs/syntax-reference.md` 从 golden EBNF 再生成 + support.toml 矩阵刷新。

**Phase B（0.34.6-10）语义决策落地**：
- 数值强制统一：双向豁免删除 → 单向 `is_numeric_coercion`（i32→i64/f64、i64→f64），
  stdlib 3 处修复（env::get_int / io::input_int / net::send）+ spec §6.13；
- W013 newtype lint + newtype 虚假承诺注释删除；`nominal_is_flow_state()` 单一事实源；
- `resolve()` release fallback 收紧为 `Type::unknown`（不再静默毒化下游）；
  31 处调用点完整迁移 zonk 登记 0.1.5（dx-backlog #1）；
- E0431 新码（escape 泄漏到边界外）+ `Any` 用户语法移除（builtin_type_names 删除）；
- `ieee_float {}` 双后端实现（域准入证）+ quote!/ast_eval checker 注册（AST 类型）。

**Phase C（0.34.11-18）Flow 补完 + 逃生舱**：
- ADR-001 实施：become/stay 删除（唯一终止符 `return State {}`）；
- View/Mutate 最小闭合（参数级强制 + payload 成员级真借用，双 checker 同步）；
- Multi-target 实施（ADR-002）：`-> A | B` 语法 + E0419 反转 + E0226 闭合；
- Fault 范围收缩（spec §3.12 实现驱动改写）+ @dense 删除 + `with` 子句删除（E0254）。

**Phase D（0.34.19-22）1.0 准备**：
- CHECKER-GAP 软测试审计：47 处 soft→hard 全处理（quote 1 处 R8 裁决豁免）；
- `unsafe_cast_protocol` 逃生舱（checker/resolved/interp/codegen 四层 + dyn fat-pointer
  存量 bug 修复）；
- E0432 泛型×线性边界拒绝 + 文档化（后续 0.34.24 收紧为容器同拒）；
- FFI 1.0 表面评估（盲审 P0 实质闭环，bindgen 残余登记 0.2）+ DX 积压登记表
  （`devdocs/v0.34/dx-backlog-0.1.5.md` 8 项）。

**Phase E（0.34.23）未评估特性评审 I**：
- parasteps / actor / protocol / capability 1.0 决策清单（`feature-decision-0.34.23.md`）：
  parasteps 收敛为结构化并发语法、actor 核心保留（runs_flow codegen 登记 0.2）、
  protocol 编译期契约保留（session 桥登记 0.2）、capability 核心保留（codegen split
  补齐）；认领 3 项（spawn_detached interp 置位 / cap split codegen / 文档漂移修正）。

### 0.34.24 审计战役（四份 audit-{codegen,flow,type,syntax} 报告驱动，原始报告存 /tmp 已被系统清理，以下条目为唯一留存记录）

**CRITICAL 全部闭环**：
- **audit-codegen C1**：multi-target turn 标签 + box 尺寸修复。
- **audit-type C1**：Any 移除后 stdlib Set dispatch 前置豁免。
- **audit-type C2**：turbofish 显式实例化绕过 E0432 线性逃逸封堵（`infer_turbofish` 补线性检查）。
- **audit-type C3**：prelude `f`/`g` 高阶参数被用户同名全局函数遮蔽误拒整文件——resolved lowering + bytecode 解析顺序对齐 checker（local 优先）。
- **audit-syntax C1**：`quote!` 内 `if let` 静默丢弃导致 E0800 栈下溢 → 编译期干净拒绝。
- **audit-syntax C2**：`if let` / `for (k, v)` 解构 native codegen 落地（desugar 到 match / let-tuple），L1 双后端目标测试 un-ignore。

**HIGH**：
- **audit-flow H3**：单组件 cap split 双后端分歧 → check 期 E0221 统一拒绝。
- **audit-codegen H2**：fallible 转移上下文泄漏进逃逸 lambda → 错型 fault return UB 消除。
- **audit-codegen H3/H1**：fault return 路径堆清理（valgrind 0 泄漏）+ 非穷尽 match 吸收对齐。
- **audit-codegen H4**：persistent 字段 Fault shadow 双后端分歧修复。
- **audit-codegen H5**：`unsafe_cast_protocol` 非 record ICE → E0713。
- **audit-type H4**：E0431 自分配后从未发射 → finalize 边界逃逸泄漏（`_`/Infer/unknown 残基）现发射 E0431（ResolveError::EscapeHatch 变分码），let-init `_` 合法边界不变。
- **audit-type H2**：容器线性逃逸封堵——`List/Option/Map<cap>` 禁止穿越泛型边界（E0432，表面类型深度线性化）；具体签名容器参数 CFG 整体视为线性，必须整体消费（E0256，valgrind 实证 drop 后 0 泄漏）。旧"容器放行"豁免被证明是 exactly-once 逃逸，契约测试翻转 + §2.3 注释与 AGENTS.md 诚实改写；元素级消费（match/for）为既有分析缺口，fail-closed 而非静默泄漏。
- **audit-type H3**（文档）：0.34.19 切片 G 名实更正——测试契约改判而非 checker 变严，"CHECKER-GAP 归零"表述修正；0.34.21 容器放行判定标注推翻。

**附带**：release 构建断链修复（`consumed_resources` cfg 门禁误加）。

### 0.34.25 收尾战役（rc-quality-gate-0.34.25a.md 切片）

**0.34.25a — L1 正确性切片（Q1/Q3/Q4）**：
- **Q1**：`match Result<T, string> { Err(msg) }` 的 Err 字符串载荷绑定误编译修复——codegen 此前把 Err 载荷当 i64 重新解释，`p_mi`/`p_mf` 均 panic；Constructor 臂按 payload_idx==2 && 非 variant_owner 推导 `err_expected_ty`（string，兼容 `Type::Result` 与 `Name("Result",[ok,err])` 两 AST 形态），`decode_payload_struct` 两调用点接入，Err 字符串经 inttoptr+load 正确取回。
- **Q3**：trait impl 方法返回 Result 的值在 let 绑定 / println 直调两条路径丢失类型（codegen 显示裸积元组 `(true, 1.5, 0)` 而 VM 显示 `Ok(1.5)`）——新增 `infer_impl_method_return_type`（type_impls 查 FuncDef.ret + fmt_type 渲染），compile_block / compile_block_last_val / compile_func_body 三处 let 绑定 type 追踪与 infer_object_type method-call 背配合接入。
- **Q4**：`ast_eval(quote!{true})` 显示分歧——Value::Bool 折叠由 i64(0/1) 改为布尔显示，VM/codegen 一致。

**0.34.25b — 泄漏切片（Q2）**：io.rs 显示缓冲（256B/次）系统性泄漏修复——marker 机制（`display_marker`/`flush_display_since`）：display 缓冲 codegen 期注册但运行期走分支/循环，顶层一次性 flush 会 free(undef)（Err 臂）并 double-free（Option Some 臂在 None 迭代）；marker 在自身无条件缓冲后取样、flush 发射在**同一运行期块**（臂尾/循环尾）使 frees 与 mallocs 同频。8 个 list 发射器 + emit_result_to_string_typed（ok/err 臂）+ emit_option_to_string 插桩。`dual_from_json` 143/0、`dual_list_option_list_tuple`/`dual_option_record_println` 转绿；p_leak 10000 循环 valgrind **0 errors**；p_dbl4 输出正确无 double-free。

**0.34.25c — mutate place fail-closed（Q5/Q6）**：mutate 实参 place 文法校验——合法 place 为 `Ident` 与单层 `Ident.field`（含 `self.field`，与现有写回机制对齐）；拒绝嵌套 place（`o.inner.value`）、非 place 实参（字面量/计算值/`bump(42)`）、同调用内重复 mutate place（别名同时借用）。E0434（非 place）/ E0435（别名）新码登记；2 个 ignored 测试转负测试（flow_features.rs 4448/4476 对应）。嵌套写回与跨调用别名追踪登记 1.x。

## [0.1.3] — 2026-08-02

### Codegen O1 优化全量回归修复（MIMI_OPT=1 双后端全绿）

- **fix(codegen): lambda 主体补全**：`emit_lambda_body` 新增 `Stmt::If`/`Assign`/`While`/`For`/`Block` 分支（此前仅参数/返回值，闭包回调全部产出 `ret i64 0`）；If 为值模式 + 返回类型宽度调整；`default_ret_value` 按 IntType 自身宽度（`count_val(xs, 2)` O0 0→1，O1 同）。
- **fix(codegen): 泛型实参强转**：`coerce_args_to_param_types` 对泛型函数跳过 `llvm_type_for` 强转（未知泛型名 fallback i64 会把 `0.0` fptosi 成 i64 0），monomorph 调用点按具体形参类型重 coerce（`coerce_args_to_function`）。`sum_float` 经 `reduce_list` 求和修复。
- **fix(codegen): reduce 内建 float 支持**：`compile_reduce_intrinsic` 重写（acc/elem/ret 类型携带、list 槽 i64 位模式 `bit_cast` 回 f64/f32、int↔float 转换）；`try_convert_loop_elem` 对 f32/f64 位模式恢复。
- **fix(codegen): tuple 含 List 返回值**：`compile_tuple_expr` 对 List 字段按值 load（`partition` 返回 `{ptr,ptr}` 与结果类型 `{{i64,ptr},{i64,ptr}}` 不符，O1 verifier 报错）。
- **fix(codegen): 分支/循环条件 i64→i1 归一化**：builtin 谓词（如 `str_starts_with`）返回 zext 的 i64，`compile_if_stmt`/`compile_while_stmt`/func.rs 第二处 if emitter 统一 `icmp ne` 归一（`br i64` 非法 IR）。
- **fix(codegen): 分支 string 字面量值归一**：`normalize_block_last_string` 将裸 string 指针分支值包裹为 `{ptr,i64}`（`first_char` 的 `{ "" }` 分支）。
- **fix(codegen): heap free 跨块 SSA 支配**：`register_heap_alloc` 将指针固化到 entry alloca 槽，两个 free 路径从槽 load（修复 `free(ptr %str_char_at_call)` 不支配 use → O1 指令选择 SIGSEGV）。`a1_verification.mimi` O1 全 10 项通过。
- **fix(codegen): trait 方法调用 string 字面量实参**：`compile_self_method_call` 对 string 形参包裹 raw 指针为 `{ptr,i64}`（`str_index_of("ll")` 经 O1 inline 展开报 `extractvalue ptr` 类型错）。
- **fix(codegen): actors.rs if-else 合并 phi 缺失 else 边默认值**（O1 verifier SIGSEGV）；**io.rs write/fopen Result 返回值修复**（上一轮遗留）。
- 验证：`MIMI_OPT=1` 与 `MIMI_OPT=0` 双路径 4513 lib + real_world 28 + real_world_cli 1 全绿，clippy/fmt 门禁全过。

### INTERP 性能深度优化 III（寄存器缓冲池化 + L1 Any 转换修复）

- **fix(codegen): L1 双后端断裂——`to_int`/`to_float` 对 `Any`（map_get 值）返回裸堆指针**：`map_get` 的值在 LLVM 层是未类型化的 i64 handle（字符串存堆指针）。codegen 的 `to_int`/`to_float`/`str_parse_int`/`str_parse_float` IntValue 分支把 handle 当整数直接返回（`to_int("3000")` → 指针值 591434576），interp 正确解析为 3000。修复：新增 runtime `mimi_any_to_int`/`mimi_any_to_float`（复用 `safe_c_string_from_handle` 启发式：<1MB 整数直通、映射页 + NUL 终止 → strtol/strtod 解析），四个 builtin 的 i64 参数统一经 runtime 判定（与既有 `to_string` Any 路径同设计；CheckedProgram 无局部变量类型目录，静态区分不可行）。`parse_c_decimal_i64` 自实现 strtol 语义（runtime libc feature 无 strtol/strtod），含 i64::MIN 饱和边界。
- **perf(bytecode): frame 寄存器缓冲区池化**：`BytecodeVM.free_regs: Vec<Vec<Value>>` 池 + `pop_frame()` 统一回收（exec_loop 落尾 / RetEarly / do_return 三处），push_frame 复用池中缓冲区（capacity 保留）。release fib(30) 0.72s → **0.57s**（-21%）；debug 被边界检查开销掩盖（3.12s）。
- **回归测试**：`dual_map_get_string_value_to_int`、`dual_map_get_string_value_to_float`（L1 双后端等价）。
- 验证：4513 lib 全绿 + real_world 28 + real_world_cli 1 + clippy/fmt 门禁全过。

### INTERP 性能深度优化 II（exec_loop 单借用重写）

- **perf(bytecode): exec_loop 热分支单借用重写**：常量/整数/浮点/比较/位运算/字符串/跳转 ~40 个 opcode 重写为「单次 `last_mut` 借用内完成读+算+写」，消除每 op 2-3 次 `get_reg`/`set_reg` 的边界检查与 `debug_assert` 开销（debug 构建下 slice/Vec 访问检查约占 40% 执行时间）。`get_int`/`get_float`/`get_float2`/`get_int2` 内联进热分支，错误消息语义逐分支保持（`expected Int/Float, got {}`）；`check_float` 内联为 `is_nan`/`is_infinite` 检查（静态 op 字符串不再 `format!`）。
- **perf(bytecode): CALL 参数寄存器提示**：`compile_expr_into`/`compile_binary_into`/`compile_unary_into`/`compile_literal_into`——CALL 参数表达式直接编译到参数槽寄存器，消除每调用点冗余 `Op::Mov`（fib 每帧 15 op → 13 op，寄存器 12 → 10）；非简单表达式安全回退 `compile_expr` + `Mov`（副作用顺序不变）。
- **perf(bytecode): push_frame 预分配**：`Vec::with_capacity(register_count)` 一次分配到位，消除 resize 的 realloc + memmove。
- **perf(bytecode): Op::Mov 同寄存器短路**：`rd == rs` 跳过克隆。
- **基准 (debug build, 本机)**：fib(30) 3.67s → **3.10s**（-15.5%）；for-range 1M 0.92s → **0.79s**（-14%）；list 循环 1M 0.96s → **0.79s**（-18%）；map 3000×2 3.80s → 3.66s（map_set 值语义深克隆主导，O(n) 克隆为语言语义固有）。
- 验证：4511 lib 全绿 + real_world 28 + real_world_cli 1 + clippy/fmt 门禁全过。

### 查缺补漏：clippy/fmt 债务清零

- **refactor(interp): InterpError 变体内容 Box<ErrorContext>**：`InterpError` 15 个 variant 从直接持有 `ErrorContext`（~128 字节）改为 `Box<ErrorContext>`（8 字节）。`Result<Value, InterpError>` 在解释器热路径上的复制成本大幅下降；消灭 ~672 个 `result_large_err` clippy warning。工厂方法（`InterpError::new`/`div_by_zero` 等）签名不变，调用点零改动。`src/interp/error.rs`、`src/interp/value.rs`。
- **fix(interp): bytecode VM/compiler 39 处 `unwrap()` 清零**：VM 新增 `cur_frame()`/`cur_frame_mut()` 不变量 helper（`mimi_debug_assert!` + ICE `unreachable!`），替换 `exec_loop`/寄存器访问/`do_ret` 中 22 处 `stack.last().unwrap()`；FuncCompiler 新增 `vars_mut`/`break_jumps_mut`/`continue_jumps_mut`/`defer_scopes_mut`/`on_failure_scopes_mut` helper，替换 17 处作用域栈 unwrap（`src/interp/bytecode/vm.rs`、`compiler.rs`）。
- **fix(interp): 剩余 10 个 clippy warning 清零**：`cloned_ref_to_slice_refs`（hof.rs `from_ref`）、`new_without_default`（BytecodeCompiler/BuiltinRegistry）、`option_map_unit_fn`（compiler.rs 2 处）、`manual_map`（compiler.rs 4 处）、`redundant_closure`（stmt.rs）。
- **门禁状态**：`cargo clippy --all-targets -- -D warnings` 从 711 errors / 672 warnings → **0 errors / 0 warnings**（CI 门禁 2 恢复全绿）。4511 lib 测试全绿，零行为变化。
- **fmt 状态**：HEAD 既有 1080 fmt diff（`src/interp/` 与 `src/runtime/mod.rs`，0.33 冲刺遗留）登记为已知债务，本次提交不混入无关格式化改动。

### Soundness 收尾：审计遗留项修复

- **fix(codegen): B9 逃逸闭包 env 泄漏（0.33 收尾审计遗留）**：
  - 根因：旧 claim 设计在 emit_return 只释放函数级 scope 顶部的共享注册项，嵌套 scope（lambda 参数 / 泛型 body / 提前 return 未回退路径）中的闭包 env 泄漏；且 claim 持久化跨 CFG 路径，跨块引用导致 SSA dominance 违规（valgrind uninitialised value）。
  - 重构为「函数边界 + 每条 CFG 路径各自发射 free」设计：`begin_function_heap_scope` / `end_function_heap_scope` / `flush_heap_scopes_to_boundary`（`src/codegen/mod.rs`）。flush 只发射 free、不弹栈、不删注册（编译期堆栈是所有路径共享的），栈平衡由 `end_function_heap_scope`（只丢弃）在函数编译结束时完成。
  - claim 一次性消费：每次 flush `mem::take` 清空 `claimed_returned_envs`，守卫只在当前块内生成 → 消除跨块 SSA 引用。
  - 覆盖全部 return 路径：emit_return / emit_implicit_return / compile_block 4 条提前返回 / compile_block_last_val / lambda 3 处 / try RejectPath / actor 提前返回 + epilogue / legacy Break-Continue；`compile_block` 结尾 heap_depth 守卫防嵌套提前 return 后重复弹栈。
  - 回归测试：`e2e_valgrind_b9_closure_env`（valgrind 零泄漏零未初始化）、`dual_b9_closure_escape_chain`、`ir_b9_closure_env_guard_on_scope_exit`、`ir_b9_closure_return_tracked_at_call_site`。
- **fix(interp): IN-H8 并发 builtin channel_recv sentinel 缺陷**：旧实现用 try_recv 自旋 + 哨兵 -1 表示超时，与合法通道数据冲突（发送 -1 被静默丢弃并替换为 0）。改为直接调用阻塞式 `mimi_channel_recv`，与 codegen 路径语义一致（`src/interp/builtins/concurrency.rs:326`）。
- **fix(runtime): CG-H16 Any 值整数标记残留**：`mimi_any_to_string` 整数不再带 `(val<<1)|1` 标记，直接存储；指针与整数靠对齐/大小/有界扫描启发式区分（`src/runtime/mod.rs:1502`，`MAX_BOUNDED_SCAN=256` C12 限制）。
- **fix(builtins): bytecode Phase D builtin 迁移补全（~55 个）**：并发 26 + Shadow/FS/Regex/Allocator 15 + 网络/工具/actor/测试缺口 14。bytecode builtins 总注册 254 个，覆盖 12 模块（`src/interp/bytecode/builtins/`）。
- **A6 状态澄清（LSP 文本搜索 → AST 位置）**：领域基础设施（Span/Origin/AstNodeMeta + `PositionMap` 字节/字符索引）已在 v0.31.1 落地，但 LSP 消费端 ~20 处文本搜索调用点（如 `state.rs:530 find_enclosing_func_in_items`）尚未迁移到 AST 位置——A6 仅基础设施部分完成，消费端迁移未完成。

### INTERP 性能深度优化（原 0.1.4-dev 周期归入）

- **perf(bytecode): 消除 O(n²) 寄存器克隆**：ListGet/MapGet/MapContains/RecordGet/ConcatStr/JmpIf/EqInt 等 12 个 opcode 消除整个集合克隆，改为借用+仅克隆元素。for-range 10k: 1106ms→53ms (20.9x)。
- **perf(bytecode): for-range 计数器循环**：编译器检测 `for i in range(a, b)` 模式，直接编译为计数器循环（不调用 range() builtin，零列表分配）。内存 O(n)→O(1)。
- **perf(bytecode): 算术/比较 Int 快速路径**：AddInt/SubInt/MulInt/DivInt/ModInt/LtInt/GtInt/LeInt/GeInt/EqInt/NeInt 重构为 `if let (Int, Int)` 首选匹配，消除 2-4 个 `matches!` 分支。for-range 1M: 21%↑。
- **perf(bytecode): push_frame 消除参数双重克隆**：`push_frame` 改为接受 `Vec<Value>` (owned)，消除内部逐元素 clone。
- **perf(bytecode): StrAppend 原地拼接**：新增 `Op::StrAppend`，检测 `s = s + expr`（仅 String 类型），`push_str` 原地追加。string concat 100k: O(n²)→140ms。
- **perf(bytecode): do_return mem::replace**：函数返回时 `mem::replace` 移出返回值（frame 即将 pop），消除返回值深拷贝。

### CODEGEN 性能深度优化（原 0.1.4-dev 周期归入）

- **perf(codegen): legacy emitter for-range() 计数器循环**：检测 `for i in range(a, b)` 调用模式，编译为计数器循环（零 malloc、零列表分配）。与 `BinOp::Range` 共享 `build_for_index_*` 基础设施。
- **perf(codegen): LLVM pass pipeline 优化**：添加 `internalize` + `globaldce` pass（消除未使用 prelude 函数）；Target machine O3→O2 对齐 pass pipeline。
- **fix(codegen): for-in-list i32 元素截断**：`convert_list_elem_i64` 对 IntType 缺少 i64→i32 截断，`for x in [1,2,3]` 段错误。添加 `build_int_truncate` 当 target bit_width < 64。

### 基准 (debug build)

| 场景 | 优化前 | 优化后 | 加速 |
|------|--------|--------|------|
| for-range 10k | 1106ms | 56ms | 19.8x |
| for-range 1M | >120s (timeout) | 922ms | >130x |
| string concat 100k | >120s (O(n²)) | 140ms | ∞ |
| fib(28) | 1.72s | 1.53s | 11% |
| nested 1k×1k | >120s | 921ms | new |

## [0.1.2] — 2026-07-29

### Phase A: Codegen 全量迁移（0.32.1–0.32.15）

- **0.32.1 Option/Result 类型进入 resolved native slice**：Option<T> → `{i1, T}` / Result<T, E> → `{i1, T, E}` LLVM 结构体降低。类型降低层（types.rs）注册 Nominal Result/Option 到 struct type 映射；eligibility gate 放行 Option/Result 类型的参数/返回值/本地绑定。+3 resolved codegen 测试。
- **0.32.2 List 构造/索引/len 进入 resolved native slice**：List<T> → `{i64 len, ptr data}` LLVM 结构体降低。eligibility gate 接受 Nominal List/Map/Set（元素类型递归检查）；ResolvedExprKind::List 字面量发射（malloc + store elements + build_list_struct）；ResolvedProjection::Index 值/左值投影放行。Map/Set → i64 handle（opaque，类型降低层注册）。+4 resolved codegen 测试。
- **Map/Set literal emission（无 sprint 标签）**：ResolvedExprKind::Map/Set 字面量发射进入 resolved emitter。Map → {i64, i64} 键值对数组 + runtime `mimi_map_create` 调用；Set → 元素数组 + runtime `mimi_set_create`。+resolved_failed_functions 追踪基础设施。3 个遗留 bug 修复（match+fstring double free、claim_match_arm_string 精确弹栈、heap scope 管理）。
- **0.32.5 用户自定义 record 类型进入 resolved native slice**：Record 构造（alloca + GEP + store 逐个字段）、Record 字段访问（Field projection lvalue/rvalue）、Record 类型 signature/body 匹配（field 名索引对齐）。+2 resolved codegen 测试。
- **0.32.6 Option/Result match Constructor pattern 进入 resolved native slice**：Match 表达式 Constructor 模式（`Some(x)` / `Ok(x)` / `Err(e)` / `None`）降低为 if-else 链 + discriminant 检查（`{i1, ...}` struct 的 0/1 标签）。+2 resolved codegen 测试。
- **0.32.7 per-function dispatch 重新激活 + ABI 修复**：修复 resolved vs legacy ABI 漂移（`{ptr, i64}` string ABI 在跨 emitter 调用时导致 SIGSEGV）。string 参数传递统一为 `{ptr, i64}` struct-by-value。恢复 per-function dispatch 为默认路径。+3 E2E 双后端测试。
- **0.32.8 for-in-list 迭代进入 resolved native slice**：`for x in list_expr` 降低为 while 循环 + index 变量 + get_list 调用。List iterable 型的 eligibility 检查。+1 resolved codegen 测试。
- **0.32.9 for-in 接受任意 List<T> 表达式作为 iterable**：不限于字面量或变量——任意元表达式（call return、projection 等）的 List<T> 类型都可作为 for-in iterable。
- **0.32.10 Try 表达式 (?) 进入 resolved native slice**：`expr?` 降低为 if-else 检查 Result/Option discriminant + early return Err/None。返回值类型校验（inner type 必须为 Result 或 Option）。+1 resolved codegen 测试。
- **0.32.11 Alias/Newtype 转换进入 resolved native slice**：AliasWrap/AliasUnwrap/NewtypeWrap/NewtypeUnwrap 转换在 eligibility 中接受（LLVM 级 identity），使使用别名和新类型的程序可通过 per-function 检查。
- **0.32.12 自定义 enum 类型进入 resolved native slice**：用户自定义 enum → `{i32 tag, i64 payload}` LLVM 结构体。Enum 类型的构造器模式匹配降低为 tag 比较 + payload 提取。+2 resolved codegen 测试。
- **0.32.13 解除 impls 程序级阻断 + source_id 精确过滤**：checker 自动生成 From/Into impls 导致所有程序 `program.impls()` 非空，0.32.8 阻断修复后仍 SIGSEGV（stdlib 模块函数 body 编译）。修复：per-function source_id 过滤——只有 entry source 文件的函数进入 resolved 编译，模块函数由 legacy emitter 编译。+source_id 精确过滤降低误伤。+2 E2E 双后端测试。
- **0.32.14 range() call 迭代 + Newtype 类型 lowering**：`for x in range(start, end)` 降低。Newtype 在类型降低层注册（transparent wrapper，LLVM repr 同 inner 类型）。+1 resolved codegen 测试。
- **0.32.15 Newtype match Constructor pattern 支持**：Newtype 模式匹配（`MyNewtype(x) => ...`）降低为 payload 解包。+1 resolved codegen 测试。

### Phase B: Codegen 迁移深化（0.32.16–0.32.25）

- **0.32.16 非捕获 Lambda/闭包进入 resolved native slice**：Lambda 表达式 → `{ptr fn_ptr, ptr env}` 闭包结构体。非捕获闭包（captures 为空）无环境指针。LocalClosure 调用通过函数指针间接调用。+heap scope 生命周期修复（free 移到 return 之前）。+2 resolved codegen 测试。
- **0.32.17 自定义 enum 构造器（Ok/Err 等复用标准库名字）**：允许用户自定义 enum variant 使用 `Ok`/`Err` 等名字（消除与标准库 Result/Option 的名字冲突限制）。variant 名称解析按类型作用域。
- **0.32.17b Option/Result builtin methods**：`is_some()`/`is_ok()`/`unwrap_or(default)` 等 builtin 方法进入 resolved native slice。方法调用 dispatch 通过 `ResolvedCallee::Builtin` 桥接到既有 `compile_builtin_call`。
- **0.32.18 解除 callee source_id 限制 + builtin shadow call**：允许从 resolved 函数调用模块函数（前向声明由 legacy emitter 注册，resolved emitter 仅发射 `call` 指令）。builtin shadow call 修复（builtin 调用链中参数传递匹配 callee 声明类型）。
- **0.32.19 call 参数 coerce 到 callee 声明类型**：修复 resolved emitter 中 call 表达式参数类型不匹配问题——callee 声明类型与被调用表达式类型做 conversion 统一。
- **0.32.20 Flow 程序 per-function dispatch 解锁**：Flow 程序不再被程序级门禁阻断。per-function eligibility 检查 transition 函数体内是否含有不支持的 node；Flow 基础设施（类型定义、前向声明）由 legacy emitter 编译。Flow transition 函数中 view/mutate borrow 参数被拒绝（指针 ABI 不匹配），`self` receiver 例外（值 ABI）。
- **0.32.21 Flow state field access 支持**：Flow state 的 record-like 字段访问（`state.field_name`）在 resolved emitter 中发射为 GEP + load。
- **0.32.22 builtin call i32→i64 参数提升 + verification IR dump**：builtin 调用参数从 i32 提升到 i64（匹配 legacy `mimi_type_to_llvm` 的 ABI）。Verification IR dump 基础设施（`--dump-verification-ir` 标志）。
- **0.32.23 ProtocolMethod 静态 trait dispatch**：ProtocolMethod 调用（ResolvedCallee::ProtocolMethod）进入 resolved native slice。静态 dispatch——编译器在 checker 阶段解析具体 callee，resolved emitter 发射直接函数调用。
- **0.32.24 actors/sessions/protocols 程序级门禁拆除**：含有 actor/session/protocol 的程序不再被程序级门禁阻断。per-function eligibility 过滤器处理函数级别的排除（actor 方法：non-User origin + qualified name；session/protocol 函数：non-User origin；函数签名含 actor/session/protocol 类型：require_scalar_type 拒绝非标称 Nominal 类型）。legacy emitter 编译 actor/session/protocol 基础设施。
- **0.32.25 count_substring builtin + List O(1) push + ABI safety**：
  - `count_substring` builtin：字符串子串计数（O(n·m) 扫描），codegen 经 LLVM `memcmp` 循环 + 解释器经 Rust `matches` 实现。双后端等价。
  - List O(1) push：`push(list, item)` 经 `mimi_list_push` runtime（amortized O(1)），不再重新分配整个数组。
  - ABI safety：编译期校验 callee 签名与 caller 期望的类型一致性，防止跨 emitter ABI 漂移。
  - +2 E2E 双后端测试。

### Phase C: 删除 legacy body + 审计（0.32.26–0.32.30）

- **0.32.26 Extern FFI 调用进入 resolved native slice**：
  - `ResolvedCallee::Extern` 现在在 per-function eligibility 检查中被接受（不再是静默回退到 legacy 的原因）。
  - resolved emitter 新增 extern 调用处理：通过 `program.extern_blocks()` 查找 extern 签名 → 获取 wrapper 函数名 → 在 LLVM 模块中查找 → 参数 coercion → 调用。
  - extern wrapper 函数由 legacy emitter 在阶段 1 前向声明，resolved emitter 在阶段 4 消费，架构一致。
  - 无新增 real_world 测试（extern 调用需要实际 C 库，不属于纯双后端等价集）。

- **0.32.27 Verifier C3 迁移：合同验证从 AST 切换到 Resolved IR**：
  - C5: `core/resolved/tests.rs` 的 `legacy_body_file()` 断言替换为 `functions().len() > 0`。
  - `resolved_expr.rs::verify_contracts_from_resolved()` 新增 math 义务处理：在 requires 断言之后、body 编码之前检查 `Stmt::Math` 表达式，验证其是否被前置条件蕴含。若全部 proven 但无 ensures，返回 `Proven` 而非 `NoObligations`。
  - `resolved_expr.rs::has_math_obligations()` 新增辅助函数。
  - `ctx.rs::verify_checked_contracts()` 新增方法：遍历 `program.callables()`，为每个有 contracts/math 的 callable 调用 `verify_contracts_from_resolved()`。
  - `ctx.rs::verify_checked()` 的 `verify_file(program.legacy_body_file())` 替换为 `verify_checked_contracts(program)`。
  - 移除 `#[allow(dead_code)]` 注解。新增 `mock_verify_checked()` 作为 C4 scaffold。
  - 主验证入口 `verify_checked` (mod.rs) 拆分为双路径：Z3 可用时走 flow verifier（仍用 raw_ast），Z3 不可用时走 `mock_verify_checked`（CheckedProgram-based，无 raw_ast）。
  - `mock_verify_checked` 现在同时处理 callables 合约和 extern block 合约。
  - `verify_ffi_checked` (mod.rs) 同样拆分为双路径。
  - `flow_verify_file_or_mock` 降级为 `pub(crate)` + `#[allow(dead_code)]`（仅被测试助手使用）。
  - 基线：4448 全量测试绿，0 退化。
  - **C1 签名的迁移**：`compile_file_with_resolved` 签名移除 `file: &File` 参数，caller 不再调用 `program.legacy_body_file()`。改为内部通过 `program.raw_ast()` 获取，封装实现细节。
  - 当前 `raw_ast()` 剩余 3 个调用点：C1 (codegen 内部)、C2 (interp)、C4×2 (verifier Z3 路径)。mock 路径和三部分验证（C3/C5/ctx）已完全脱离。

- **0.32.28 C1/C4 内部保留决策（文档 sprint）**：
  - C1（codegen 第五遍）永久保留 `raw_ast()` 内部调用：AST body 注册 + ineligible body class 编译需要 raw AST。
  - C4（verifier Z3 路径）永久保留 `raw_ast()` 调用：Flow verifier 的 Z3 编码定义在 AST Expr 节点上。
  - CHANGELOG 补全 Phase A（0.32.1–0.32.15）、Phase B（0.32.16–0.32.25）全部 sprint 记录。

- **0.32.29 legacy_body_file()→raw_ast() 私有化 + compile_func→compile_func_legacy（不可逆）**：
  - `legacy_body_file()` 重命名为 `raw_ast()`，添加完整架构文档（C1/C2/C4 三个永久 consumer 的理由）。旧名删除（所有 caller 已迁移）。
  - `compile_func` 重命名为 `compile_func_legacy`，添加文档说明第五遍架构：resolved native emitter（第四遍）处理 eligible subset，legacy emitter（第五遍）处理永久 ineligible body classes（capturing lambdas/generics/async/extern ABI/view-mutate borrow params）。skip guard（`count_basic_blocks != 0`）防止双重发射。
  - 所有 consumer 调用点添加显式理由注释（C1 codegen/C2 interp/C4 verifier）。
  - 禁止新代码调用 `raw_ast()`：声明层数据通过 typed accessor 获取。
  - clippy/fmt 修复：`and_then(|x| Some(y))` → `map(|x| y)`（resolved/mod.rs:2863）。
  - 基线：4448 lib + 13 real_world + 28 cli 全绿，clippy 0 warnings，fmt 0 diffs。

### Phase D: 缺口审查 + 填补（0.32.31–0.32.35）

- **0.32.31 Slice 表达式进入 resolved native slice**：`xs[start:end]` view 语义（不拷贝数据），构建新 `{i64 len, ptr data}` struct 指向原有 buffer 偏移处。索引 clamp 到 [0, list_len] 防止 OOB 指针算术。
- **0.32.32 Old 表达式进入 resolved native slice**：合约 `old(x)` 在 codegen 为 identity（合约运行时擦除，只有 verifier 区分 old 语义）。
- **0.32.33 Comprehension 进入 resolved native slice**：`[value for pattern in iterable if guard]` 降低为预分配 buffer + 循环 + guard 过滤 + 计数 + 构建结果 list。
- **0.32.34 OptionalChain 进入 resolved native slice**：`receiver?.field` 降低为 discriminant 分支 + 字段投影 + PHI 合并。Some/Ok → 投影字段包装 Some；None/Err → 返回 None。
- **0.32.35 Callable（一等函数值）进入 resolved native slice**：`ResolvedCallee::Function` 返回 LLVM 函数指针（`GlobalValue.as_pointer_value()`）。仅 User-origin 非 qualified 函数。
- **0.32.35b NestedCallable 语句修复**：嵌套函数声明标记在 eligibility 中放行（no-op），修复含嵌套函数的程序被错误拒绝。

### Phase E: 性能基线（0.32.37–0.32.38）

- **0.32.37 性能基线基础设施**：`benchmarks/` 目录（fib/mandelbrot），Mimi codegen/interp + C (gcc -O2) + CPython 四路对比。`run.sh` 自动编译/运行/计时/异常检测（>2x 偏差标红）。
- **0.32.38 性能异常分析**：Mimi/C ≈ 4x（MIMI_OPT=1）。根因：SD-7 checked arithmetic（每个 add/sub/mul 走 `llvm.sadd.with.overflow` + overflow 检查 + trap 分支，3-4 指令 vs C 的 1 指令）。设计决策非 bug。MIMI_OPT 默认关闭（需 `MIMI_OPT=1` 启用 O2）。

---

## [0.1.1] — 2026-07-28

### 审计修复（发布前深度审计）

- **P1-1 Origin::User 过滤**：`require_resolved_native_program()` 添加 `Origin::User` 检查。non-User-origin 函数（stdlib 导入/runtime 生成）触发 all-or-nothing 路径拒绝，强制走 per-function dispatch。修复窄触发 SIGSEGV（纯函数 + `use std::xxx` 程序）。
- **P1-4 per-function LLVM verify**：`compile_subset()` 每个成功发射的函数调用 `llvm_fn.verify()`。验证失败视为发射失败，回退到 legacy emitter。防止无效 LLVM IR 溜过。
- **P2-2 死代码删除**：`compile.rs:888` `let _ = &flow.persistent_fields` 无操作引用删除。

### Phase H: RC + 发布（0.31.57–0.31.58）

- **CODEGEN Typed Resolved IR 迁移 S8–S13（per-function dispatch 激活）**：
  - S8: String ABI 统一（`{ptr, i64}` struct，消除 resolved/legacy 双 ABI 漂移）
  - S8b: Builtin string 返回值包装（`wrap_builtin_string_result`，raw ptr → `{ptr, i64}`）
  - S9: Per-function dispatch 激活 + Tuple ABI 统一（sub-64-bit int → i64，匹配 legacy `mimi_type_to_llvm`）
  - S10: E2E 双后端测试 `real_world_per_function_dispatch`（fib/divmod eligible，sum_list ineligible via List）
  - S11: 文档化 traits/impls/externs 阻断根因（impl method body 调用 builtin 传 `{ptr,i64}` struct 但 builtin 期望 raw ptr → SIGSEGV）
  - S12: `compile_resolved_subset` 移入 `compile_file_inner`（setup 之后、legacy body pass 之前），确保所有符号声明先于 resolved 发射
  - S13: Origin-based filtering（`Origin::User(_)` 检查）+ 精确文档化剩余阻断
- **Silent fallback → verbose warning**：per-function resolved emitter fallback 不再静默。`MIMI_VERBOSE=1` 输出逐函数 fallback 详情和聚合计数。默认输出不变。满足 0.1.1 硬门禁：codegen dispatch 路径无 silent fallback。
- **退出标准修订**：`devdocs/v0.31/README.md` §9 "只消费 Typed Resolved IR" → 精确分层描述（声明层全量 typed IR / 函数体 per-function dispatch + 显式 legacy arm）。SD-6 `legacy_body_file()` 删除推迟到 0.1.2 (0.32)。

### Phase H: RC1 阻断修复（0.31.57，进行中）

- **CODEGEN Typed Resolved IR 迁移 S0/S1**：新增 `codegen::resolved` 生产 emitter，首批 primitive scalar leaf callable 直接消费 `CheckedProgram` 中的 `ResolvedCallable` / `ResolvedTypeId` / `ResolvedLocalId` / `ResolvedCallee`；支持 canonical 类型降低、稳定 local identity、标量运算/转换与直接函数调用。生产入口仅对静态 eligibility 通过的 body class 切换；typed emitter 一旦入选则 fail-closed，禁止失败后回退 raw AST。毒化 `legacy_file` 的回归测试已证明该 cohort 不读 surface body。
- **CODEGEN Typed Resolved IR 迁移 S2–S4（控制流 + builtin + 类型扩展）**：resolved native emitter 逐步扩展至覆盖：If/Block/Scope 表达式、While/Loop/For(Range)/Break/Continue 语句、`ResolvedCallee::Builtin` 桥接 `compile_builtin_call`（含 print-family `pending_print_arg_types` 设置）、NumericNarrowChecked 通用数值转换（int↔float、truncate/extend/fptosi/sitofp）、String 类型（opaque ptr ABI）+ 字符串字面量 + println、Constant 目录查询发射、程序级门禁放宽（允许 type_defs 声明）。已知 checker 限制：resolved body lowering 不支持 range iterable（TOOL-RESOLUTION-001）、资源分析不认 break CFG 路径。
- **CODEGEN Typed Resolved IR 迁移 S5–S7（聚合 + FString + Match）**：Tuple 构造（alloca+gep+store）、Tuple 投影（extractvalue / GEP 链）、Tuple 解构模式（`let (a,b) = expr`）；全 primitive 类型放行（消除 eligibility 与 type lowering 白名单漂移）；no-op 规范语句（Drop/Contract/Math）；FString text-only 全局常量 + interpolation snprintf 方案（i32/i64/f64/string/bool 格式符映射）；Match 表达式链式 if-else 降低（Literal/Wildcard/Binding pattern + guard AND）。18 项 resolved codegen 测试全绿；e2e 验证 divmod/sum_digits/classify/f-string 程序双后端等价。已知 L1 cosmetic 限制：float FString 格式精度差异（snprintf %g vs interpreter Display）。
- **CODEGEN 迁移结构门禁**：`src/codegen/resolved/` 禁止调用 `legacy_body_file()` / `compile_file()`，禁止导入 surface body AST（仅允许复用无语义身份的 `BinOp` 运算符枚举）。未迁移 body class 仍保留显式 legacy arm，不计入 Typed IR 完成度。
- **测试并发文档校正**：全量日常门禁改为 `cargo test -- --test-threads=4`（性能优化后约 42 秒）；Z3 验证专项仍使用单线程，极端内存受限环境可退回单线程。
- **Native `List<string>` ABI 修复**：`mimi_list_to_string` 不再读取 native codegen 两字段 `{len, data}` 布局中不存在的 `element_kind` 尾字段；`std::array` 的真实 List 返回 API 补齐 codegen 类型识别。修复 `println(array_slice(...))` 输出地址而非字符串的 silent miscompilation，并用 real-world 双后端回归覆盖 slice/take/drop/concat。
- **路线图机器契约修复**：校验器识别 `soundness`/`completeness` milestone kind；R1/R2/R3 审查不变量不再冒充语言 requirement ID；README/RC 分册同步到权威 `last = 58`。

### Phase F: Soundness（0.31.49–0.31.54）

- **0.31.49 类型系统 `let _ =` 漏洞修复（T-1~T-4）**：push() 统一检查（`push(list_of_i32, "hello")` 被拒绝）；slice 索引整数验证；range 表达式整数验证（防御深度）；reduce() 签名验证（累加器/元素/返回类型三路统一）。Slice<X> 兼容 X（slice view 运行时强制）。+6 L2 测试。
- **0.31.50 结构性缺口修复（T-6/T-7/T-8）**：resolve() 静默回退添加 debug_assert!；resolve_type 10 个所有权 wrapper 递归解析内部类型（Shared<MyAlias> 别名解析）；ExternFunc 泛型替换保留 ABI 标记（不再静默转为 Func）。
- **0.31.51a SD-7/SD-8 整数 Trap**：add/sub/mul 使用 LLVM 溢出内建（llvm.sX.with.overflow）+ mimi_trap_overflow；div/mod 检查除零（mimi_trap_div_by_zero）和 MIN/-1（mimi_trap_div_overflow）。之前除零返回 0（静默），现在 Trap（显式失败）。21 个黄金文件重新生成。
- **0.31.51b SD-9/10/12 浮点语义统一**：SD-9 codegen 浮点 NaN/Inf trap（fcmp UNO + llvm.fabs + OEQ vs Inf，mimi_trap_float_not_finite）；SD-10 解释器 float == 改为精确比较（消除 epsilon L1 divergence）；SD-12 json get_float → Result<f64, string>（消除 0.0/0.0 NaN 哨兵）+ str_parse_float 拒绝 "NaN"/"inf"。
- **0.31.52 Runtime 安全（R-1/R-4）**：ERROR_HANDLER 函数指针存储 UB 修复（AtomicPtr<ErrorHandler> → AtomicPtr<()> + usize round-trip）；Capability 表 thread-local 限制文档化。
- **0.31.53 Verifier 健全性（V-2）**：不可编码的 requires/ensures 改为 fail-closed（返回 NotInTrustedSubset），防止约束不完整时产生假 Proven。
- **0.31.54 Interpreter/Quote（Q-1/I-4）**：QuotedAst::Return 设置 early_return（之前是空操作）；QuotedAst::For 每次迭代 push_scope/pop_scope（修复循环变量泄漏）。

### Phase D: 工具与隔离（0.31.42–0.31.44）

- **0.31.42 TyErr 毒药类型 + Z3 报错翻译**：`TyErr` 毒药类型防止级联错误刷屏（18 文件 / 31 match arms）；`VerifStatus::plain_language()` + `hint()` + `icon()` 将 Z3 验证状态翻译为用户可读语言。+12 测试。
- **0.31.43 SD-1 is_linear 结构化标记**：`ResolvedType::Nominal { is_linear: bool }` 替代字符串匹配；`NominalTypeId::nominal_is_linear()` 单一真相源（`state:` 前缀 / `SessionChan` 后缀）；`intern_type()` 设置标记，`substitute()` 保留，`canonical()` 不含（派生属性）。6 文件 / 23 处 pattern match 修复。+6 测试。
- **0.31.44 SD-3 #[errno] 属性**：块级 `#[errno] extern "C" { ... }` 传播到所有函数；函数级 `#[errno] func ...` per-function 控制。`ExternFunc.returns_errno` / `ResolvedExternFunc.returns_errno`。过渡期保留 `ERRNO_CHECK_FUNC_NAMES` 名称猜测（1.0 删除）。15 文件。+6 测试。
- **0.31.44 SD-4 fork() → 信号守卫**：删除 `FORK_LOCK` / `check_multithreaded_fork` / `call_ffi_with_fork_isolation*`（−457 行）；新增 `signal_guard.rs`（+263 行）：SIGSEGV/SIGABRT/SIGBUS/SIGILL/SIGFPE 信号处理器 + `sigsetjmp`/`siglongjmp` 恢复点 + thread-local jmp_buf。GUARD_LOCK 全局 Mutex 序列化 + IN_GUARDED_CALL thread-local 重入检测（防 C 回调 → Mimi → FFI 死锁）。+7 测试。
- **0.31.44 ResolvedExpr Z3 编码**：`src/verifier/resolved_expr.rs`（+758 LOC）：`resolved_to_z3_int/real/bool` 三路编码 + `verify_contracts_from_resolved()` 端到端合约验证。P0 健全性修复：body 编码失败 → `NotInTrustedSubset`（防假 Disproven）；encoding_failures > 0 → 无条件 `NotInTrustedSubset`。P1：result 变量类型推断（Int/Real/Bool）+ bool 等式编码（int→real→bool 三级回退）。+10 测试。

### Phase E: 冻结与 RC（0.31.45–0.31.46）

- **0.31.45 DEBUG 周期 + Interpreter dual-path**：`run_dual_path()` 同时运行 AST 和 Resolved 解释器，比较结果；`DualPathResult` 枚举（Match / ResolvedUnsupported / ResolvedFailed / BothFailed / AstFailedResolvedOk / Mismatch）；`values_equal_for_test()` 深度值比较。28 项 dual-interpreter 等价测试覆盖：基础算术、控制流、函数、递归、数据结构、模式匹配、记录/变体、Option/Result、字符串、合约、推导、闭包。关键发现：ResolvedInterpreter 支持闭包，不支持 FFI。
- **0.31.46 最终敌对审查**：
  - **Trap Tests**（+26 测试）：IEEE-754 边界（NaN/Inf/除零/负零）、整数溢出（i32/i64 MIN/MAX/wrap）、OOB 访问（列表/字符串/嵌套）。关键发现：Mimi 不遵循 IEEE-754 除零语义（抛 DivisionByZero 而非 Inf/NaN）；NaN 比较抛错而非返回 false；列表支持 Python-style 负索引。
  - **type_folder 测试补充**（2→18 项）：RemapFolder / CollectVarsFolder / NamedSubstitutionFolder 全覆盖，含 ForAll binder 遮蔽、链式替换、环检测（P1-13）。
  - **诊断输出确定性**：Checker::check() 对 errors/warnings 按 (start_line, start_col, message) 排序，消除 HashMap 迭代非确定性。
  - **0.31.30–38 深度审查修复**：BUG-1 c_header.rs 数字开头标识符净化死代码；BUG-2 codegen_e2e.rs 测试空转；LIE-1/LIE-2 文档撒谎修正；GAP-2 serialize.rs 静默降级警告；GAP-5 AllocLedger DoubleAlloc 检测；GAP-7 wire.rs contains_handle_depth 保守返回。

### Phase C: Component 边界（0.31.30–0.31.36 完善）

- **0.31.30 Component IR**（COMPONENT-IR-001）：`ComponentIr` 单一语义源（identity/exports/imports/types）+ `AbiGenerator` 类型化注册表（`register_core_runtime_abi` 覆盖 RC/list/map/set/string/io/concurrency/fs/time ~40 函数）+ `.mimiabi` JSON 序列化 round-trip + BLAKE3 防篡改哈希。Codegen debug 构建对发射的 runtime 调用做 Component IR 校验。
- **0.31.31 Native ABI + 代际 handle**（COMPONENT-HANDLE-001）：`HandleRegistry` nominal generational handle 运行时——kind 标签 + runtime owner + generation 计数 + 并发 lease；destroy 要求零 lease 且递增 generation（ABA 防御）；拒绝 StaleGeneration/WrongKind/WrongRuntime/UnknownSlot/LeasesOutstanding；Mutex 线程安全零 unsafe。String 表面迁移到胖指针（MimiString/MimiSlice）+ 零拷贝 `mimi_string_as_slice`。11 个测试含 8 线程并发压力。
- **0.31.32 稳定检查点**：`checkpoint.rs` 无新 surface——ABI 布局 probe（offset 越界/非单调/对齐校验）+ `.mimiabi` round-trip 不动点 + allocator provenance ledger（wrong-side/double-free/unknown free/leak 检测）+ handle lease race 高并发压力（generation 复用安全）。11 个测试。

### io.rs 格式化 dispatch 重构（Sprint A1–A2 完成）

- 46 个 `emit_map_*` 近重复函数合并为参数化 `emit_map_container_product_to_json` + `product_tuple_arity` 辅助函数（-1544 行）。
- `extract_print_arg` 的 Map/Set 分发树（1886 行手工展开 if-else）替换为 `resolve_container_product` 递归类型解析器（-1693 行）。
- io.rs: 14658 → 11421 行（-22%），双后端 735/0 等价确认。

### FLOW-IDENTITY-001 状态身份（0.31.8 完成）

- **E0421 状态不可伪造**：非根 flow state 在 transition 体外构造被拒绝。根状态（flow 第一个 state）构造不受影响（Flow 构造器语义）。3 个新测试。
- **E0422 名义状态区分**（warning）：跨流同名义状态 payload 兼容时发出 warning，提示使用限定名 `flow::<flow_name>::<state_name>`。
- **E0423 线性 generation**：flow state 变量被 transition 消费后，后续使用被静态拒绝。Checker 层 `consumed_flow_vars` 追踪 + `lookup_var` 拦截；解释器 `mark_moved` safety net。2 个新测试。`flow_counter.mimi` 和 `flow_codegen_chain` 中的 use-after-transition 已修正。

### FLOW-TURN-001 原子 Turn（0.31.9 完成）

- **`fails E` 语法**：transition 签名支持 `-> Target fails ErrorType`，声明可回滚失败路径。Lexer 新增 `fails` 关键字，Parser 解析 `fails E`，AST `TransitionDef.fails: Option<Type>`。
- **E0424**：transition 体内 `?` 无 `fails E` 声明时静态拒绝。
- **Rejected 路径（解释器）**：`?` 失败时 transition 返回 `Err((source_payload, error))`，source generation 归还调用方。修复 `early_return` 泄漏 bug（transition 体内 `?` 的 `early_return` 不再穿透到调用方）。4 个新测试。
- **Codegen fail-closed**：`fails E` transition 中 `?` 在 codegen 报 `CompileError::Unsupported`（E0722），防止静默产生错误行为。无 `?` 的 `fails E` transition 正常编译。
- **`transition_fails_types` 基础设施**：Checker 存储每个 transition 的 `fails E` 类型。
- **返回类型 `Result<Target, (Source, E)>`**：Checker 注册 `fails E` transition 返回类型为 `Result<Target, (Source, E)>`；Resolved IR `ResolvedTransition.fails` 字段 + canonical 签名同步包装；IR Lower `transition_fails` 标志使 return 语句期望内层 Target 类型；Interpreter 成功路径包装 `Ok(v)`。测试更新为 match Ok/Err 模式。
- **`become`/`stay` 显式 terminal 关键字**：`become Expr` 构造目标状态并结束 transition（等价于 return）；`stay` 返回 source 状态不变（自环终端）。全栈实现：Lexer/Parser/AST/Checker/IR Lower/CFG/Interpreter/Codegen（block.rs + func.rs）。5 个新测试（含双后端等价）。
- **Codegen Rejected 完整镜像**：`fails E` transition 中 `?` 在 codegen 不再 fail-closed（E0722 已移除）。Rejected 路径构造 `Err((source, error))` 并返回；成功路径包装 `Ok(target)`。`transition_to_func` 返回类型变为 `Result<Target, (Source, E)>`。2 个新双后端测试。
- **Draft isolation**：transition 体内 `self` 为不可变参数（`mut_: false`），source 在 Rejected 时原样归还。原子 turn 保证：transition 要么成功返回 Ok(target)，要么失败返回 Err((source, error))，不存在中间状态泄漏。
- **已知限制**：codegen match on Result with record payloads 不支持对绑定变量的字段访问（`var_type_names` 未注册 Ok payload 类型名），需用 `Ok(_)`/`Err(_)` 模式。

### 0.31.10 稀疏图 + typed Fault + 显式 reset/recover（进行中）

- **Per-Flow typed Fault**：`fault ErrorType` 声明语法，注入的 Fault 状态携带 `error: ErrorType` 字段。回退 transition 自动填充默认值。2 个新测试（含双后端）。
- **@sparse 稀疏图**：`@sparse` bare annotation 跳过 N×M fallback 注入。未声明的 (state, event) 对产生编译时错误而非自动路由到 Fault。2 个新测试。
- **显式 reset/recover**：用户自定义 `transition reset(Fault) -> State` / `transition recover(Fault) -> State` 覆盖自动注入的系统动词。2 个新双后端测试验证覆盖行为。
- 待实现：progressive Main 真 lowering（main 函数体作为 transition body 参与 Resolved IR lowering）。

### 0.31.11 Actor runs Flow（进行中）

- **`actor Name runs FlowName` 语法**：AST `ActorDef.runs_flow: Option<String>`，Parser 解析 `runs` soft keyword，Checker 验证引用的 flow 存在（E0402）。
- **Interpreter 集成**：`ActorInstance` 新增 `runs_flow` + `flow_state` 字段。spawn 时初始化 flow_state 为 root state（默认值）。Worker thread dispatch：`runs_flow` 设置时消息路由到 Flow transition table（从 flow_state 提取当前状态名 → 查找 (from_state, event) 匹配的 transition → `eval_flow_transition` 执行原子 turn → 更新 flow_state）。
- **mut 字段禁止**：`runs_flow` actor 的 `mut` 业务字段被 E0402 拒绝（状态由 Flow 携带）。
- **测试**：`actor_runs_flow_dispatch_through_transition` 验证 Zero→Positive→Positive 多 turn 累积（s3.n == 2）。
- 待实现：Codegen actor runs flow（需要 tagged-union state 存储 + state-dependent dispatch，与当前 flat-struct actor 模型不兼容，需专门设计）。

### 审查修复（0.31.9–0.31.11 事后审查）

- **C1 `block_returns_on_all_paths` 不认识 `Become`/`Stay`**：match 缺少分支导致 E0255 误报，CLI 拒绝合法 `become`/`stay` 代码。测试因 `run_source_result` 跳过 checker 而漏网。修复：添加 `Stmt::Become(_) | Stmt::Stay => return true`。
- **C2 Rejected 路径 error 双重包装**：`eval_try` 设 `early_return = Some(Err(e))`（完整 variant），Rejected 路径再包一层 `Err((source, Err(e)))`。修复：解包 variant 取内层 error 值。
- **H1 CFG 中 `Stay` 是 no-op**：`stay` 后的代码在 CFG 中仍可达，影响 ownership 分析。修复：标为 `Terminator::Return`。
- **H2 `Stay` 无类型验证**：注释声称 checker 验证 source 类型匹配，但无代码。修复：`self` 类型与返回类型 unify 失败时 E0209。
- **附加：`become`/`stay` 不再设 `early_return`**：仅 `?` 使用 Rejected 信号，避免 `become` 在 `fails E` transition 中误触发 Rejected 路径。

### 0.31.12 Typed Session Residual（完成）

- **E0425 scope exit 检查**：函数结束时，非 `end` residual 的 session endpoint 被拒绝。endpoint 必须完成协议（send/recv/close）或显式 return/transfer。
- **E0426 use-after-alias**：`let b = a`（a 是 session endpoint）后，a 被标记为 consumed，再用 a 触发 E0426（线性消费）。
- **Alias residual 转移**：`let b = a` 将 residual 从 a 转移到 b，a 的 residual 被移除。
- **Branch merge 一致性**：if/else 两分支的 session residual 必须一致才能 merge，分歧时 E0425。无 else 分支时保守恢复 pre-branch 状态。
- **测试基础设施（H3）**：新增 `checked_run_source_result` / `checked_compile_and_run`（checker + 后端），迁移 0.31.9–0.31.11 测试到 checked helper。
- 6 个新测试：alias 转移、use-after-alias、scope exit 拒绝/通过、branch merge 一致/分歧。

### 0.31.13 Resource exactly-once（进行中）

- **Session endpoint 函数参数 move**：session endpoint 作为函数参数传递时消费 residual，修复 E0425 误报，正确报 E0304 (moved after consumed)。
- **Session 线性回归验证**：double-close (E0304)、branch partial consume (E0425)、move-to-function (E0304) 三个场景确认 CFG dataflow 覆盖。
- **Cap 闭包 capture**：已有 TransferChild 分析 + E0304 强制（`ownership_checker_rejects_implicit_nested_capability_capture`），无需新增。
- 3 个新测试：session_double_close_rejected、session_branch_partial_consume_rejected、session_endpoint_move_to_function_rejected。
- **追加 A — Flow 状态别名追踪 + shared/ref 拒绝**：
  - `let b = s0`（s0 是 flow state）消费 s0，后续使用 s0 触发 E0423（对标 session E0426 机制）。
  - `shared`/`local_shared`/`weak`/`weak_local` 包装 flow state → E0427 拒绝（线性资源不允许多重引用）。
  - `let ref r = flow_state` → E0427 拒绝（借用隐含原值仍可用，违反线性）。
  - 删除 `consumed_flow_vars.remove(name)` shadowing 清除逻辑——shadowing 不重置线性消费（保守策略，0.31.16 CFG place 追踪修正）。
  - `flow_state_type_names: HashSet<String>` 注册所有 flow state 类型名（qualified + unqualified）。
  - CFG `is_linear()` 预留 transition `self` 跳过逻辑（0.31.16 启用 FlowStateSet + state: Nominal）。
  - 5 个新负测试。4098 测试全绿。
- 待实现：cross-turn exactly-once（Flow transition 间资源跟踪）、Fault path 资源清理。

### 0.31.14 Static Protocol Stable（进行中）

- **移除 deprecated `protocol_methods`**：spec 标记 `[removed]`，从 builtins/inference/codegen/interpreter 全部清除。Protocol 是纯编译期拓扑检查，不需要运行时反射。
- **Protocol 测试迁移**：4 个双后端测试迁移到 checked helper。
- **追加 A — Protocol conformance × 线性检查**：
  - Protocol state payload 线性匹配：protocol 声明线性 payload (Cap, SessionChan) 时，flow state 对应字段必须也是线性类型，降级 → E0427。
  - 3 个新测试：alias bypass (E0423)、alias target valid、payload downgrade (E0427)。
- 待实现：permission/effect 约束检查、fault 暴露策略、版本握手（需 Component IR，Phase C）。

### 0.31.15 Canonical Semantic Trace（基础设施完成）

- **TraceEvent / TraceCollector / compare_traces**：canonical 语义追踪基础设施（`src/trace.rs`），记录 Transition + Fault 事件，5 个单元测试。
- **Interpreter 集成**：`trace_collector` 字段 + `eval_flow_transition` 中 transition/Fault 事件记录。
- **`run_source_with_trace` 测试 helper**：trace 收集测试基础设施。
- **追加 A — 所有权转移事件 + generation 失效记录**：
  - `OwnershipTransfer` 事件：记录 flow state 所有权转移时刻（from_var → to_var，generation 失效精确位置）。
  - `LinearViolation` 事件：记录运行时 use-after-move 安全网诊断路径。
  - `compare_traces()` 扩展：generation_before/generation_after 参与比较（happens-before DAG generation 边）。
  - 3 个新单元测试。4104 测试全绿。
- 待实现：session/actor trace 记录、双后端 trace 比较测试。

### 0.31.16 Flow 状态 CFG 级线性（核心完成）

- **`is_linear()` 纳入 Flow 状态**：`FlowStateSet`（multi-target 结果）和 `state:` 前缀 Nominal（individual flow state）在 CFG dataflow 中是线性资源。
- **Auto-droppable**：Flow 状态代表数据，scope exit 时可安全丢弃（与 Cap/SessionChan 必须显式消费不同）。`ActionEmitter` 收集 flow state 局部变量作为 droppable 集合，`validate_return_resources` 跳过。
- **Transition `self` 隐式消费**：`build_resource_catalog` 和 `introduce_parameters` 跳过 transition 首参。
- **`_` 前缀 auto-drop**：`_d` 等 intentionally unused 变量不报 E0256。
- **`consumed_flow_vars` 保留为诊断增强层**：E0423 带 transition 名（比 CFG 的 E0304 更友好），CFG dataflow 是强制层。
- **Channel/Mutex/Atomic 遗留**：builtin 函数（整数 handle），非 ResolvedType Nominal，`is_linear()` 无法覆盖，留给后续类型表示升级。
- 4104 测试全绿。

### 0.31.19 攻击审查 I（完成）

- **审查范围**：0.31.16–18 闭环后的地基层（Flow 线性完备性、generation 失效、Actor×Flow 边界、Session×Flow 交互、双后端一致性、错误信息）。
- **P1 发现 + 修复**：tuple 构造 flow state 不消费原变量（`let t = (s0, 42); Counter::inc(s0)` 通过）→ `infer_tuple_expr` 加 `is_flow_state_type` 检查，E0427 拒绝。
- **线性完备性审查结果**（10 条攻击路径全部静态拒绝）：

| 攻击路径 | 诊断 | 层 |
|----------|------|-----|
| use-after-transition | E0423 | checker |
| alias chain (`let b = s0; let c = b; use(b)`) | E0423 | checker |
| self-loop double-use | E0423 | checker |
| function param move | E0304 | CFG |
| closure capture | E0427 | checker |
| list literal | E0427 | checker |
| map value | E0427 | checker |
| **tuple construction** | **E0427** | **checker (本次修复)** |
| shared/ref wrapping | E0427 | checker |
| shadowing no-reset | E0423 | checker |

- **错误信息质量**：E0423 带 transition 名 + help 文本；E0427 带类型名 + help 文本；E0304 (CFG) 无 transition 名（consumed_flow_vars 诊断增强层补充）。
- **Known limitation**：Channel/Mutex/Atomic 非 ResolvedType Nominal，is_linear() 无法覆盖；consumed_flow_vars 名字追踪保留为诊断层。
- **P0 = 0**。审查报告归档于 CHANGELOG。
- 4109 测试全绿。

### 0.31.18 证据同步与回归扫描（完成）

- **language-support.toml 全面更新**：implementation_version 更新至 0.1.1-dev (sprint 0.31.17)，8 个 requirement evidence 更新。
- **Clippy/fmt 修复**：`!is_ok()` → `is_err()`，`run_source_with_trace` dead_code allow。
- **回归扫描**：4108/0/10 全绿，clippy 0 warnings，fmt clean，real_world 70/70 run / 69/70 build。
- **Deferred 项清点（0.31.8–17）**：

| 项 | 来源 | 状态 | 去向 |
|---|---|---|---|
| progressive Main 真 lowering | 0.31.10 | 推迟 | 需 Resolved IR 级设计（broke 23 golden IR tests） |
| Codegen actor runs flow | 0.31.11 | 推迟 | 需 tagged-union state 存储设计 |
| cross-turn exactly-once | 0.31.13 | 推迟 | Flow transition 间资源跟踪，需 CFG 扩展 |
| Fault path 资源清理 | 0.31.13 | 推迟 | 需 Fault 路径 resource analysis |
| permission/effect 约束 | 0.31.14 | 推迟 | 需 Component IR (Phase C) |
| fault 暴露策略 | 0.31.14 | 推迟 | 需 Component IR (Phase C) |
| 版本握手 | 0.31.14 | 推迟 | 需 Component IR (Phase C) |
| session/actor trace 记录 | 0.31.15 | 推迟 | 需 interpreter session/actor 路径集成 |
| 双后端 trace 比较测试 | 0.31.15 | 推迟 | 需 codegen trace 收集 |
| Channel/Mutex/Atomic is_linear() | 0.31.16 | 降级 known limitation | builtin 函数（整数 handle），非 ResolvedType Nominal |
| consumed_flow_vars 删除 | 0.31.16 | 降级 known limitation | 保留为诊断增强层（E0423 带 transition 名） |

- **Consumer 迁移审计**：interp/codegen/verifier 三后端声明层（签名/Flow transition/Actor/Session/Protocol/ownership/CFG）从 CheckedProgram 安装；**函数体仍经 `legacy_body_file()` 消费 raw AST**（`interp/mod.rs:323`、`codegen/compile.rs:596`、`verifier/ctx.rs:1130`、`verifier/mod.rs:49,125`）。`legacy_body_file()` 为 `pub(crate)` 可见性，阻止 crate 外新 consumer 回退。函数体 Resolved IR 迁移按 body class 追踪于 0.31.8–0.31.19。

### 0.31.17 高阶交互闭环（完成）

- **闭包 × Flow**：lambda 内引用外层 flow state 变量 → E0427 拒绝（"linear resource cannot be captured by closure"）。`lambda_depth` + `lambda_param_names` 追踪，区分参数和 capture。Lambda 参数中的 flow state 合法。
- **集合 × Flow**：`[s0, s1]` list literal → E0427 拒绝。Map literal value 为 flow state → E0427 拒绝。
- **修复既有坏测试**：`flow_state_lambda_param_accepted`（fn 类型语法不合法）、`flow_state_in_set_rejected`（set{} 语法不存在）。
- 4108 测试全绿。

## [0.1.0] — 基线稳定 - 2026-07-23

### 止血 II 收尾 + 版本管理切换 + 架构重构

- **版本管理切换**：外部版本从 `mimi-v0.31.X` 切换为纯 semver（`0.1.0`、`0.1.1`、...、`1.0.0`）。旧 `mimi-v*` tag 保留为开发历史，不再新增。内部 sprint 仅体现在 commit message 中。
- **架构重构（0.1.0 收尾）**：
  - `src/runtime/mod.rs` 拆分（24105→18142 行，14 个模块）：regex/lexer/crypto/fs/binary_io/future/ffi_test/concurrency/actor/quote/net/shadow_mte/capability/env 抽出，机械拆分不改语义，419 个 `#[no_mangle]` 符号全导出、4053 测试绿。硬共享簇（map/set/list/string/json ~180 extern fn）函数交错且互引，作为耦合核心保留 mod.rs。
  - `src/core/resolved.rs` 拆分（12702→8551 行）：目录化为 resolved/mod.rs，`#[cfg(test)] mod tests`（4129 行）分离到 resolved/tests.rs。identity/catalog/walk 生产代码边界模糊且重度耦合，作为耦合核心保留 mod.rs。
- 止血 II 修复项（按信任链排序，逐项完成后登记）：
  - **F1 测试 oracle**：删除进程级 `GLOBAL_STDOUT_CAPTURE` 全局槽与 `resolve_stdout_buf` fallback，消除并行测试 stdout 串扰。
  - **silent error 止血**：codegen 12 处 `let _ = build_store/build_call` 改传播；`test_sandbox` spawn 失败如实报告。
  - **文档真值**：`AGENTS.md` §13/§0 重新对齐（函数体层仍经 `legacy_body_file()`、线性能力 0.1.1 前零强制）。
  - **CI 门禁**：`LLVM_SYS_181_PREFIX` 修正、clippy `--all-targets`、分级门禁、unsafe SAFETY baseline 锁定。
  - **测试质量**：清零走过场测试、`v1_4` 家族强制 L1、real_world golden（增量）。

> 开发历史：1863 commits，66 个 `mimi-v*` tag（v0.12.0–v0.31.6），38 天（2026-06-15 至 2026-07-22）。
> 详细施工记录见 `devdocs/archive/` 和 git log。

### 里程碑

- **CheckedProgram 语义中枢**：唯一语义真值源，持有 canonical 签名、Flow transition 表、Actor/Session/Protocol 目录、ownership action summaries、CFG。
- **Typed Resolved IR**：ResolvedFunction/ResolvedFlow/ResolvedTransition/ResolvedActor 等 canonical 声明（12.7k LOC）。
- **HM Unification**：undo trail + TypeScheme + zonk；泛型调用 fresh instantiate。
- **CFG/Ownership 分析**：per-callable 控制流图 + stable-ID CallableCfg + 线性资源 ledger（Introduce/Move/Drop/Return + borrow）。
- **止血 I/II**：测试 oracle 修复、silent error 传播、文档真值对齐、CI 门禁强化、Clippy 基线清零。
- **双后端等价**：4063 测试（4053 passed / 0 failed / 10 ignored），69 个 real_world 程序双后端 68/69 通过（`flow_test_macros.mimi` 为 interpreter-only，不参与双后端比对）。
- **Flow 范式**：38 项白皮书能力全部达成（v0.29 冻结），双后端 stdout 等价。
- **stdlib**：io/fs/strings/collections/json/csv/crypto/maps/mymath/net/time/datetime/env/testing/regex/template/set。
- **工具链**：mimi check/run/build/verify/fmt/lint/lsp/init/add/install/tree。

### 已知限制（0.1.0 基线）

- 线性能力仅有分析，零用户可见强制（exactly-once 闭环排入 0.1.1）。
- Flow 转移无原子 terminal model（atomic turn 排入 0.1.1）。
- Session 端点运行时可退化为整数（typed residual 排入 0.1.1）。
- Component IR / ABI / Wire 不存在（排入 0.1.1 内部 Phase C）。
- 函数体仍经 `legacy_body_file()` 消费 raw AST（迁移排入 0.1.1）。

---

## Pre-0.1.0 时代摘要

> 详细施工日志（v0.1.0–v0.31.6，1863 commits，66 个 `mimi-v*` tag）保留在 git 历史中
> （`git log -- CHANGELOG.md`），本地归档副本见 `devdocs/archive/CHANGELOG-pre-0.1.0.md`。

| 时代 | 版本范围 | 日期 | 主题 |
|------|---------|------|------|
| 原型 | v0.1.0–v0.7.0 | 06-15 ~ 06-17 | 解释器 + 类型检查器 + CLI 原型 |
| 筑基 | v0.12.0–v0.20.1 | 06-23 | 控制流、函数、类型系统、stdlib 基础 |
| 补全 | v0.21.0–v0.27.6 | 06-24 ~ 06-26 | JSON、LSP、pipe/loop、Z3 验证器、结构化并发、安全审计 |
| 使用驱动 | v0.28.0–v0.28.37 | 06-27 ~ 07-03 | 7 语言 FFI、profiler、bindgen、包管理器；Feature Bugs 清零 |
| Flow 范式 | v0.29.0–v0.29.41 | 07-03 ~ 07-12 | 编译器内部 Flow 替换（Parser→Lexer→Loader→LSP→Interp→Verifier→Checker）+ 语言级 Flow 语义 + 白皮书 38 项能力全部达成 |
| 止血 | v0.30.0 | 07-14 | 0 新 Feature — 15 项架构债务清零（sprintf→snprintf、路径安全、malloc 检查等） |
| 语义中枢 | v0.31.0–v0.31.6 | 07-15 ~ 07-22 | CheckedProgram / HM unification / CFG / Resolved IR / 止血 I/II → 汇入 0.1.0 |
