# S2: deadline and responsibility handoff

`timeout_handoff.mimi` makes the handoff explicit: it checks an absolute
`timestamp_ms()` deadline at a cooperative work boundary. An expired request
id and bounded checksum input are packed into the retry-owner channel once,
and the original path returns a timeout receipt. The retry owner consumes the
job and calls the frozen C checksum ABI; the originating path has no second
retry. The baseline W4 service already keeps accepted worker futures and
joins them after stop.

The current surface cannot interrupt a blocking `tcp_recv` or forcibly cancel
a running task. This anchor therefore places the deadline check at the
request-work boundary and exercises explicit responsibility transfer. It does
not claim that `sleep_ms` is cancellable or that the baseline service's receive
is deadline-aware. A complete runtime timeout guarantee still requires a
cancellable timed receive/operation plus generation-checked task ownership.

Build the focused policy fixture against the same trusted C ABI after building
the shared helper. The source is an evolution anchor; the baseline network
service remains the full two-client/stopping workload.

```sh
cc -std=c11 -Wall -Wextra -Werror -fPIC -shared \
  tests/fixtures/v02_app/w4/oracle/w4_trusted.c \
  -o /tmp/mimi-w4-build/libw4_trusted.so
mimi build tests/fixtures/v02_app/w4/changes/S2/timeout_handoff.mimi \
  --link-search /tmp/mimi-w4-build --link-lib w4_trusted \
  -o /tmp/mimi-w4-build/w4_s2_handoff
LD_LIBRARY_PATH=/tmp/mimi-w4-build /tmp/mimi-w4-build/w4_s2_handoff
```

Expected output, if the current task/channel and C-import shapes are accepted,
is `Timeout;owner=retry-queue` and `RetryComplete id=731 checksum=417`.
