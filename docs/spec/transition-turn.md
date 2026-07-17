# Flow Transition Turn 1

> Normative profile: `mimi-flow-turn-1`
> Binding source: `docs/language-requirements.toml`. This header is descriptive; the manifest is authoritative.

## 1. 配置

一个 turn 的抽象配置为：

```text
Turn = <instance, source_state, generation, payload, event, locals,
        resource_ledger, effect_ledger, transaction_log>
```

进入 turn 时独占消费 source generation。payload 被移入不可发布的 draft；外部观察者在 terminal commit 前仍只能看到上一已提交 generation。

## 2. 求值阶段

1. 校验 instance、source state、generation 和 event。
2. 建立 draft、局部 scope 和 resource ledger。
3. 按 Resolved IR 执行 body；字段写只修改 draft。
4. 每条 effect 在 effect ledger 中记录其可回滚类别。
5. 恰好执行一个 terminal action。

## 3. Terminal outcome

```text
Become(target, payload) -> publish generation + 1 and target
Stay(payload)           -> publish generation + 1 and same nominal state
Fault(variant, payload) -> publish typed Fault generation
Rejected(error)         -> discard draft and return original source generation
```

- `Rejected` 只能来自 transition 签名声明的 rollback error。
- `?` 在 transition 中 lower 为 `Rejected`，在普通函数中 lower 为 callable return。
- body 正常结束、执行两个 terminal、或存在 terminal 后可达语句均为静态错误。
- multi-target 若启用，`Become` 的 target 必须携带 closed nominal tag。

## 4. 原子性与 effect

内存中的 Flow payload 发布是原子的。外部 effect 不可能被通用回滚伪造：

- reversible：有机器可执行 undo，失败时逆序执行；
- idempotent：携带稳定 operation key，可安全重试；
- compensated：携带显式 saga compensation，补偿失败成为 secondary typed failure；
- irreversible：必须先进入明确中间状态，再由完成事件推进。

可回滚 turn 中出现未分类不可逆 effect 是静态错误。

## 5. 资源结算

Terminal 前 checker 必须证明 ledger 中每个线性资源恰好一个最终动作。`Rejected` 恢复 source payload 中原有资源；turn 内新资源按 scope 规则 drop/return。`Fault`、panic absorption、reset 和 recover 不得用默认值替代外部资源。

## 6. 稀疏图和动态边界

未声明 `(state,event)` 不产生 transition。静态调用不存在即 check error。网络/FFI/IPC 输入依次执行 decode、schema、Protocol、generation/state 校验；失败返回 typed boundary error，只有显式 Flow 策略可以将其升级为 Fault。
