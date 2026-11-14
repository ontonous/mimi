# S1: same ABI, replacement checksum implementation

`w4_trusted_s1.c` is a replacement trusted library for the baseline. It keeps
the exact exported symbol and C signature `int32_t w4_checksum(int32_t)` and
computes the same hand-frozen oracle vectors with a bounded sum-of-odds loop.
The listener, loopback restriction, error sentinel, and trace ABI stay the
same. Build the baseline service against this file in place of
`oracle/w4_trusted.c` and rerun the complete host driver; do not change the
request oracle to whatever the replacement happens to emit.

The checksum result for 0..999 is unchanged. Values outside that range still
return -2. The change is implementation-only and preserves wire behavior,
ownership, and failure routing.

Build the baseline service against the replacement file and pass that library
to the same independent host oracle; keep the baseline JSON and event fixtures
unchanged:

```sh
cc -std=c11 -Wall -Wextra -Werror -fPIC -shared \
  tests/fixtures/v02_app/w4/changes/S1/w4_trusted_s1.c \
  -o /tmp/mimi-w4-build/libw4_trusted_s1.so
mimi build tests/fixtures/v02_app/w4/baseline/src/main.mimi \
  --link-search /tmp/mimi-w4-build --link-lib w4_trusted_s1 \
  -o /tmp/mimi-w4-build/w4_service_s1
LD_LIBRARY_PATH=/tmp/mimi-w4-build \
  /tmp/mimi-w4-build/w4_host_driver --native \
  /tmp/mimi-w4-build/w4_service_s1 \
  /tmp/mimi-w4-build/libw4_exports.so \
  /tmp/mimi-w4-build/libw4_trusted_s1.so
```
