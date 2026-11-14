# S3: endpoint scope tightening

The accepted set is an explicit closed scope tag for 127.0.0.1 and ports
24000..24100. A listener request for port 0 asks the adapter to select the
first available port in that interval. The Mimi wrapper rejects any other
scope or port before calling C. The C adapter repeats the check before
delegating to the trusted listener/connector, so bypassing the wrapper still
cannot create an external endpoint.

`endpoint_scope_oracle.c` freezes the rejected-scope and rejected-port results
and wraps `socket`, `bind`, `connect`, and `listen` to prove all rejected cases
return before those system calls. The single allowed ephemeral listener must
perform exactly one socket/bind/listen sequence and bind inside the interval.
Build this oracle with the baseline C implementation and the S3 adapter:

```sh
cc -std=c11 -Wall -Wextra -Werror \
  -Wl,--wrap=socket,--wrap=bind,--wrap=connect,--wrap=listen \
  tests/fixtures/v02_app/w4/oracle/w4_trusted.c \
  tests/fixtures/v02_app/w4/changes/S3/w4_endpoint_scope_s3.c \
  tests/fixtures/v02_app/w4/changes/S3/endpoint_scope_oracle.c \
  -o /tmp/w4-s3-endpoint-oracle
/tmp/w4-s3-endpoint-oracle
```

The Mimi adapter can be built against the same combined trusted helper:

```sh
cc -std=c11 -Wall -Wextra -Werror -fPIC -shared \
  tests/fixtures/v02_app/w4/oracle/w4_trusted.c \
  tests/fixtures/v02_app/w4/changes/S3/w4_endpoint_scope_s3.c \
  -o /tmp/mimi-w4-build/libw4_trusted_s3.so
mimi build tests/fixtures/v02_app/w4/changes/S3/endpoint_scope.mimi \
  --link-search /tmp/mimi-w4-build --link-lib w4_trusted_s3 \
  -o /tmp/mimi-w4-build/w4_s3_scope
LD_LIBRARY_PATH=/tmp/mimi-w4-build /tmp/mimi-w4-build/w4_s3_scope
```

The existing driver tests the baseline loopback bind. S3 additionally runs
this oracle and the full baseline regression after the transport change.
