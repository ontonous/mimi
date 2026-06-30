#!/bin/bash
# v0.28.11 LSP Manual Verification Script
# Sends LSP requests to `mimi lsp` via stdio with proper Content-Length headers
set -e

MIMI_BIN="${1:-target/debug/mimi}"
if [ ! -f "$MIMI_BIN" ]; then
  echo "Building mimi..."
  LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo build --bin mimi 2>/dev/null
fi

# Write a JSON-RPC message with Content-Length header to a file
write_msg() {
  local json="$1"
  local len=${#json}
  printf "Content-Length: %s\r\n\r\n%s" "$len" "$json"
}

INPUT_FILE=$(mktemp)
OUTPUT_FILE=$(mktemp)

# Build the input with proper LSP headers
write_msg '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' >> "$INPUT_FILE"
write_msg '{"jsonrpc":"2.0","method":"initialized","params":{}}' >> "$INPUT_FILE"
write_msg '{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///vtest.mimi","text":"type Person { name: string, age: i32 }\nfunc main() -> i32 {\n    let p: Person = Person { name: \"Bob\", age: 30 }\n    println(p.name)\n    0\n}"}}}' >> "$INPUT_FILE"
write_msg '{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":"file:///vtest.mimi"},"position":{"line":0,"character":6}}}' >> "$INPUT_FILE"
write_msg '{"jsonrpc":"2.0","id":3,"method":"textDocument/completion","params":{"textDocument":{"uri":"file:///vtest.mimi"},"position":{"line":0,"character":0}}}' >> "$INPUT_FILE"
write_msg '{"jsonrpc":"2.0","id":4,"method":"textDocument/rename","params":{"textDocument":{"uri":"file:///vtest.mimi"},"position":{"line":2,"character":8},"newName":"person"}}' >> "$INPUT_FILE"
write_msg '{"jsonrpc":"2.0","id":5,"method":"shutdown","params":null}' >> "$INPUT_FILE"
write_msg '{"jsonrpc":"2.0","method":"exit","params":{}}' >> "$INPUT_FILE"

echo "=== Sending LSP requests to mimi lsp ==="
cat "$INPUT_FILE" | "$MIMI_BIN" lsp > "$OUTPUT_FILE" 2>/dev/null || true

echo "=== Responses ==="
cat "$OUTPUT_FILE"
echo ""

# Parse and validate
echo "=== Validation ==="
RESP=$(cat "$OUTPUT_FILE")

# Check each response
validate() {
  local id="$1" label="$2" pattern="$3"
  if echo "$RESP" | grep -q "Content-Length:" && echo "$RESP" | grep -q "\"id\":$id"; then
    echo "✅ $label"
  else
    echo "❌ $label (id=$id not found)"
  fi
}

validate 1 "Initialize" ""
validate 2 "Hover" ""
validate 3 "Completion" ""
validate 5 "Shutdown" ""

echo ""
echo "=== Cleanup ==="
rm -f "$INPUT_FILE" "$OUTPUT_FILE"
echo "Done."