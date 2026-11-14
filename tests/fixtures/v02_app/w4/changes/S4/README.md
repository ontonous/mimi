# S4: transport package split

The package `offline-release/.mimi/deps/w4_transport` owns the public transport
functions for listener creation, accept, byte receive/send, newline framing,
and bound-port lookup. Its module path is imported as
`w4_transport::transport`; the application entry and JSON/checksum policy stay
in `offline-release/src/`. The pinned lock entry records the complete cached
package checksum, and the package still calls the same trusted, scope-checked
C ABI.

Use the `offline-release/README.md` instructions from a copied deployment
directory. The check must first run `mimi install --offline --frozen`, then
build with the exact lock and run the shared host oracle from a working
directory outside both the source fixture and package cache. The host oracle
replays the same two-client, bad-request, C-error, in-flight-stop, worker-join,
and listener-close scenario against the deployed executable.
