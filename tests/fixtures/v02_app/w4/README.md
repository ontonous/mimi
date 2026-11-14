# W4 loopback request service fixture

This directory contains the W4 acceptance program and its independent native
oracle. It is intentionally a small but real local service workload: a Mimi
server accepts two concurrent persistent clients, keeps malformed JSON local
to one request, maps a trusted C checksum error into a response, then accepts
a cooperative stop while one worker is still in flight and joins both workers
before it exits.

The top-level `w4_*_probe.mimi` files remain legacy smoke inputs for the shared
P6 evaluator. This directory is the full W4 story and is self-contained so a
reviewer can build it without changing that shared evaluator.

## Frozen request and event contract

Requests and responses are newline-delimited UTF-8 JSON. A client waits for
one response before sending its next line. The framing helper accepts partial
TCP reads and caps a line at 2048 bytes. Pipelined requests are deliberately
not part of this fixture.

The host driver starts two pthread clients together. Client A sends request
101 with value 20 and a 750 ms work delay. Client B sends malformed JSON, then
the same valid value 20, then value 1000 to exercise the C library's error
return, followed by a configurable soak set. After the server reports that
request 101 is in flight, the host sends request 900 with `op=stop`. The server
acknowledges the stop, closes the listener, and awaits both worker handles.
The fixed oracle requires `STOP_ACCEPTED(900) < RESPONSE(101) < DRAINED`; the
stop cannot be reported as complete while accepted work still has an owner.

The independently authored checksum table is in `oracle/w4_expected.json`.
For the baseline implementation, `checksum(20)=417`, `checksum(21)=458`, and
values outside 0..999 return the explicit C error sentinel `-2`; Mimi turns
that into `ChecksumRange` without aborting the service. The bad-JSON response
is followed by a successful request on the same client connection.

## Native build and host oracle

Run these from the repository root after a Mimi binary is available. They are
instructions only; this fixture was not built or run as part of its creation.

```sh
mkdir -p /tmp/mimi-w4-build
cc -std=c11 -Wall -Wextra -Werror -fPIC -shared \
  tests/fixtures/v02_app/w4/oracle/w4_trusted.c \
  -o /tmp/mimi-w4-build/libw4_trusted.so

mimi build tests/fixtures/v02_app/w4/baseline/src/exports.mimi \
  --shared -o /tmp/mimi-w4-build/libw4_exports.so
mimi build tests/fixtures/v02_app/w4/baseline/src/main.mimi \
  --link-search /tmp/mimi-w4-build --link-lib w4_trusted \
  -o /tmp/mimi-w4-build/w4_service

cc -std=c11 -Wall -Wextra -Werror -pthread -ldl \
  tests/fixtures/v02_app/w4/oracle/w4_host_driver.c \
  -o /tmp/mimi-w4-build/w4_host_driver

LD_LIBRARY_PATH=/tmp/mimi-w4-build \
  /tmp/mimi-w4-build/w4_host_driver \
  --native \
  /tmp/mimi-w4-build/w4_service \
  /tmp/mimi-w4-build/libw4_exports.so \
  /tmp/mimi-w4-build/libw4_trusted.so

LD_LIBRARY_PATH=/tmp/mimi-w4-build \
  /tmp/mimi-w4-build/w4_host_driver \
  --vm "$(command -v mimi)" \
  "$(pwd)/tests/fixtures/v02_app/w4/baseline/src/main.mimi" \
  /tmp/mimi-w4-build/libw4_exports.so \
  /tmp/mimi-w4-build/libw4_trusted.so
```

The host driver checks the C oracle vectors and endpoint denial directly,
loads `w4_factorial` and `w4_status` from the Mimi shared library, then starts
the service and runs the concurrent socket scenario. It accepts either
`--native SERVICE` or `--vm MIMI SOURCE` and uses the same host schedule and
oracle in both modes. Optional trailing arguments set a nonnegative soak count
(default 64) and restart rounds (default 1); each client performs that many
additional request/response exchanges before closing. The driver records
event order, child peak RSS, and maximum observed descriptor/thread counts in
its stdout.
`offline-release/README.md` runs the same host oracle against the S4
deployment copy after `mimi install --offline --frozen`.

The exports-only target has no `main`; that is intentional. It verifies the
library target containing C-export bodies, internal recursion, and a wrapper
without turning the absence of an executable entry point into a language
failure. `w4_factorial` is called both recursively from Mimi and through
`dlsym` from C. `w4_status` calls the same recursive body, and the service also
calls `internal_export_probe` from that source module.

## ABI and resource boundary

| Boundary | Frozen behavior | Ownership/effect |
|---|---|---|
| `w4_open_listener(scope, port, backlog)` | Accepts only scope tag 1; binds `127.0.0.1`; port 0 requests an ephemeral port | `@net @blocking`; successful fd ownership transfers to Mimi; close once with `close_fd` |
| `w4_listener_port(fd)` | Returns the bound loopback port or a negative error | Local `getsockname`; fd remains owned by the caller |
| `w4_checksum(value)` | 0..999 returns the frozen checksum; other values return -2 | Deterministic, allocation-free scalar C ABI; no pointer ownership |
| `w4_trace(code, detail)` | Writes one fixed-size event record to stderr | Diagnostic I/O only; no application ownership |
| accepted client fd | Passed once to a spawned handler | Handler closes it on EOF or local receive/send failure; parent only joins the task |
| listener fd | Kept by the service coordinator | Closed after stop acknowledgement and before worker joins |

The service never calls `tcp_listen`, because the current stdlib wrapper binds
`INADDR_ANY`. The trusted C listener accepts only a closed scope tag, not an
arbitrary address string; the companion S3 helper applies the same check to
outbound loopback connections. This prevents a request from smuggling a DNS
name or non-loopback address through the network boundary.

Current Mimi has no statically enforced network endpoint capability type and
does not mark the `i64` file descriptor as a linear resource. The trusted C
adapter enforces the allowed endpoint at runtime, while the source explicitly
assigns each fd to one close site and the host oracle checks its release. This
is a concrete boundary of the current language surface, not a compiler-proved
capability guarantee. The service admits exactly two concurrent worker sockets
and caps each persistent connection at 256 requests; the host driver exercises
repeated requests and repeated service restarts within those limits.

The trusted C library is the explicit trusted computing base for listener
creation, endpoint checks, checksum behavior, and event logging. It has no
callbacks into Mimi and receives no Mimi heap pointers. The independent host
driver is a separate process and is not linked into the service.

## Frozen negative ABI cases

`oracle/w4_ffi_contract.json` records the expected checksum error mapping, a
wrong call-site argument, a C prototype mismatch, and an ownership-mismatch
target. The wrong call-site argument must fail before execution. Current Mimi
extern declarations do not compare their prototype with a C header and do not
carry a view/owned/destructor clause for strings, so both foreign-boundary
cases remain explicit unresolved targets rather than claimed rejections.
`negative/wrong_external_abi_declaration.mimi` and
`negative/borrowed_c_string_as_owned.mimi` are not safe to run; they preserve
the future static ABI/ownership diagnostics as acceptance targets.

## SEQ-W4 anchors

Each step has an isolated starting artifact under `changes/`:

| Step | Artifact | Required invariant |
|---|---|---|
| S1 | `changes/S1/` | Replace only the C implementation, retaining symbol and scalar ABI; compare against the same C vectors and the request transcript |
| S2 | `changes/S2/` | Add a request deadline; `Timeout` transfers the request id to the retry owner exactly once, and shutdown still joins accepted work |
| S3 | `changes/S3/` | Narrow listener/connect scope and prove rejected endpoints fail before a socket syscall |
| S4 | `offline-release/` | Move transport into the pinned `w4_transport` package, install from the local cache with `--offline --frozen`, then launch the built artifact from a non-source working directory |

These are source and oracle anchors, not an executed cumulative chain. Parent
hashes, route receipts, hidden checks, L1/L2/L3 runs, soak measurements, and an
independent maintainer handoff remain unrecorded until their owning evaluator
captures them.
