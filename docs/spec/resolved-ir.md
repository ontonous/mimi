# Typed Resolved IR 1

> Normative profile: `mimi-resolved-ir-1`
> Binding source: `docs/language-requirements.toml`. This header is descriptive; the manifest is authoritative.

## 1. 流水线边界

```text
Surface AST -> normalization -> checker -> CheckedProgram<ResolvedIR>
                                          |-> interpreter
                                          |-> native codegen
                                          |-> verification lowering
                                          |-> component exporter
```

只有 checker 可以把名称、overload、权限、effect、错误传播和 Session residual 解析为稳定语义。后端不得查找“第一个同名候选”、按 variant 名猜错误、按布局猜状态或在 unsupported 时继续生成代码。

## 2. 稳定身份

每个 ID 是 module-qualified nominal identity，不由表中位置决定：

```text
ItemId        = { package, module, kind, declaration_name, disambiguator }
FlowId        = ItemId(kind=flow)
StateId       = { flow: FlowId, state_name }
TransitionId  = { flow: FlowId, event_name, source: StateId }
ProtocolId    = ItemId(kind=protocol)
SessionId     = ItemId(kind=session)
ComponentId   = { package, component_name, abi_major }
```

序列化 ID 使用 canonical UTF-8 qualified name 加 schema-version hash；进程内可使用 dense index，但 artifact 必须保留 canonical identity。

## 3. CheckedProgram

```text
CheckedProgram {
  semantics_version,
  source_hash,
  items: [ResolvedItem],
  calls: NodeId -> ResolvedCall,
  transitions: NodeId -> ResolvedTransitionCall,
  permissions: NodeId -> Permission,
  effects: ItemId -> EffectSummary,
  sessions: NodeId -> { before, after },
  resources: EdgeId -> [ResourceAction],
  backend_requirements: [CapabilityRequirement],
  origins: NodeId -> Origin,
}
```

`Origin` 为 `User(span)`、`Desugared(parent, rule)`、`PrototypeFallback(parent, rule)` 或 `RuntimeSystem(rule)`。所有诊断必须最终映射到 `User(span)`。

## 4. Resolved Transition Call

```text
ResolvedTransitionCall {
  transition: TransitionId,
  source_type: FlowInstance<FlowId, StateId, Generation>,
  targets: ClosedSet<StateId>,
  terminal_set: ClosedSet<TerminalKind>,
  rollback_error: Option<TypeId>,
  argument_conversions: [CheckedConversion],
}
```

解析必须恰好命中一个 `TransitionId`。零命中是用户诊断；多命中是歧义诊断；后端收到缺失或重复 resolution 是 compiler error，不允许恢复猜测。

## 5. Session 与资源

- 每次 endpoint 操作记录 residual before/after；未知 residual 不是 `Any`，而是 check failure。
- 每个线性值在 CFG 边上记录 `Move(target)`、`Return`、`Drop`、`TransferSession` 或 `TransferChild`。
- branch merge 只有在 residual 和 ownership action 兼容时成立。
- `view`/`mutate` borrow 在调用返回边终止；逃逸或 transition 跨越 borrow 是错误。

## 6. Backend Capability Gate

后端声明版本化 capability set。编译目标所需 capability 不在 set 时，checker/build 在生成 IR 前失败：

```text
UnsupportedForBackend {
  requirement_id,
  construct_span,
  backend,
  missing_capability,
  available_profile_or_migration,
}
```

warning、no-op、首目标、零值和 sentinel 都不是合法降级。Experimental feature 必须显式启用，且其 capability 缺失同样 fail-closed。
