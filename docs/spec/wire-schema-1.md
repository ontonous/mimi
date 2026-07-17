# Mimi Wire Schema 1

> Normative profile: `mimi-wire-schema-1`
> Binding source: `docs/language-requirements.toml`. This header is descriptive; the manifest is authoritative.

## 1. Envelope

```text
Envelope {
  wire_major, wire_minor,
  component_id, protocol_id, schema_hash,
  message_id, request_id,
  trace_id, parent_call_id,
  authority_id, expected_revision,
  capability_credential,
  payload,
}
```

编码必须 canonical：字段按 numeric ID，整数/长度有唯一编码，UTF-8 必须有效，map key 顺序固定，NaN 若未来允许必须 canonicalize。Native pointer、handle token、allocator ID、callback ctx 和 padding 永不编码。

## 2. 身份与演进

Message/field/variant ID 发布后不得复用。Unknown optional field 默认保留/跳过；unknown required message、variant 或 capability scope fail-closed。Required->optional、类型 widening 和尾部新增只有在兼容表允许时成立。

握手交换 wire version、schema hash、Protocol ID、支持的 message/variant bitmap、limits 和 capability audience。未协商成功不得发送业务消息。

## 3. 限制与安全

每个 profile 冻结 message bytes、nesting depth、collection length、string bytes、decompression ratio 和 outstanding requests 上限。超限返回 typed `SchemaLimitExceeded`，不能截断。

Request ID 在 audience/connection epoch 内唯一。重复请求按声明的 idempotency/replay policy 拒绝或返回缓存结果；out-of-order 只在 Session residual 允许时接受。

## 4. Authority 与冲突

外部事实携带 authority identity 和 revision；command 携带 expected Flow generation 或 fact revision。冲突返回 typed `RevisionConflict { expected, actual, authority }`。Projection 带 revision 但不获得提交权。

## 5. Error、cancel 和 trace

Wire result variant ID 与 Native ABI 的逻辑 BoundaryResult registry 一致，但 payload 使用 wire schema。Cancel 是 request，terminal `Cancelled`/`Completed` 恰好一个。所有 request/callback/cancel 延续 trace ID 和因果 parent ID。
