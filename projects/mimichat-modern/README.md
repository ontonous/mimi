# mimichat-modern

0.1.7 Phase D dogfood slice for the mimichat rewrite.

Covers:
- Flow `ChatSession` transcript lifecycle
- Actor `RoomService`
- Real-thread `Channel<i64>` worker via `spawn` / `await`
- Large payload `ChatMessage` record

## Dogfood-driven fix (0.37.54)

The first version of this project kept only a scalar `messages` counter in
`ChatSession` because resolved Flow transition bodies rejected explicit
`List<string>` local annotations with `TOOL-RESOLUTION-001`. The dogfood
failure was used as a regression target and fixed: Flow transition bodies now
persist every explicit type annotation owned by the transition, so
`let xs: List<string> = []` / `let mut xs: List<string> = []` compile through
the resolved typed-body lowering.

The project now stores a real `List<string> transcript` inside the Flow state
and copies it through `join` / `accept`, serving as a live regression case for
the fix.

## Dogfood-driven fix (0.37.55)

When the project added a real TCP echo service, a Flow transition named
`accept` collided with the networking builtin `accept(fd)`: the legacy emitter
treated every Flow transition as an EventId enum variant, so `accept(fd)` in
the plain server function compiled as a flow event constructor instead of the
socket accept call. The fix scopes bare `StateId`/`EventId` variant
construction to Flow transition bodies and clears per-Flow compiler state
after `compile_flow`.

The project now includes `server_echo` / `client_echo` built on the std
`net` wrappers (`tcp_listen` / `tcp_accept` / `tcp_connect` / `tcp_send` /
`tcp_recv`) and runs a real echo round-trip through `spawn` + `await` in the
native built binary, making this a live regression case.

With `net` added to the resolved module-body allowlist, the `std::net`
wrapper functions are compiled through the resolved native slice; the
project stays at 0 legacy fallback in `make test-dispatch-zero`.

## Concurrency dogfood (0.37.56)

The net service is now a concurrent multi-client echo slice:

1. `server_echo(port, ready)` publishes readiness on a `Channel<i64>`.
2. It accepts three connections and starts one `echo_handler(client)` per socket
   with `spawn` from inside the server task.
3. `main` receives the ready signal, starts three `client_echo` tasks, and awaits
   all four task results.

This exercises nested spawn, channel-based startup synchronization, and several
concurrent socket lifecycles in the real-thread runtime. Native binary output:
`net: 0/0/0/0`.
