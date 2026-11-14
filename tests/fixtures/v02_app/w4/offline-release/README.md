# Offline locked deployment fixture

The package ships a seeded `.mimi/deps/w4_transport` cache entry and a
`mimi.lock` checksum for that exact package tree. The application imports the
dependency through the package loader; it does not reach back into the source
fixture. The dependency owns listener creation, accepted-socket wrappers,
partial-read newline framing, and complete-write framing. Application JSON and
checksum policy stay in the app. The lock checksum is computed with the
package manager's sorted relative-path/content SHA-256 format.

After the repository Mimi binary and the trusted C library have been built,
copy this directory to a deployment path that is not under the repository:

```sh
mkdir -p /tmp/mimi-w4-build
cc -std=c11 -Wall -Wextra -Werror -fPIC -shared \
  tests/fixtures/v02_app/w4/oracle/w4_trusted.c \
  -o /tmp/mimi-w4-build/libw4_trusted.so
sha256sum /tmp/mimi-w4-build/libw4_trusted.so \
  > /tmp/mimi-w4-build/native-dependencies.sha256
mimi build tests/fixtures/v02_app/w4/offline-release/src/exports.mimi \
  --shared -o /tmp/mimi-w4-build/libw4_exports.so
cc -std=c11 -Wall -Wextra -Werror -pthread -ldl \
  tests/fixtures/v02_app/w4/oracle/w4_host_driver.c \
  -o /tmp/mimi-w4-build/w4_host_driver
cp -a tests/fixtures/v02_app/w4/offline-release /tmp/w4-deployed
cd /tmp/w4-deployed
mimi install --offline --frozen
mkdir -p /tmp/w4-deployed/out
mimi build src/main.mimi \
  --link-search /tmp/mimi-w4-build --link-lib w4_trusted \
  -o /tmp/w4-deployed/out/w4_offline
cd /tmp
LD_LIBRARY_PATH=/tmp/mimi-w4-build \
  /tmp/mimi-w4-build/w4_host_driver --native \
  /tmp/w4-deployed/out/w4_offline \
  /tmp/mimi-w4-build/libw4_exports.so \
  /tmp/mimi-w4-build/libw4_trusted.so
```

That host-driver invocation replays the same two-client concurrent schedule,
bad-JSON local failure, C error mapping, stop-during-in-flight handoff, worker
joins, and listener-close check against the deployed executable. It starts
from `/tmp`, outside both the source fixture and the package cache. The helper
library remains an explicit native deployment dependency; the command records
the built library's SHA-256 separately because the Mimi package lock does not
pin a C shared object.
