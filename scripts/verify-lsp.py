#!/usr/bin/env python3
"""v0.28.11 LSP Manual Verification Script.
Sends LSP requests to `mimi lsp` via stdio with proper Content-Length headers."""
import subprocess, json, os, sys

MIMI = sys.argv[1] if len(sys.argv) > 1 else "target/debug/mimi"
if not os.path.exists(MIMI):
    subprocess.run(["cargo", "build", "--bin", "mimi"], check=True,
                   env={**os.environ, "LLVM_SYS_181_PREFIX": "/tmp/llvm-wrapper"})

def encode_msg(obj):
    body = json.dumps(obj, ensure_ascii=False)
    # LSP: Content-Length header + \r\n\r\n + body.
    # mimi LSP server also reads one extra byte after body (the trailing
    # \n that some transports insert between messages).  We append \n so
    # that extra read does not eat into the next Content-Length header.
    return f"Content-Length: {len(body)}\r\n\r\n{body}\n"

requests = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
    {"jsonrpc": "2.0", "method": "initialized", "params": {}},
    {"jsonrpc": "2.0", "method": "textDocument/didOpen",
     "params": {"textDocument": {"uri": "file:///vtest.mimi",
                 "text": 'type Person { name: string, age: i32 }\nfunc main() -> i32 {\n    let p: Person = Person { name: "Bob", age: 30 }\n    println(p.name)\n    0\n}'}}},
    {"jsonrpc": "2.0", "id": 2, "method": "textDocument/hover",
     "params": {"textDocument": {"uri": "file:///vtest.mimi"},
                "position": {"line": 0, "character": 6}}},
    {"jsonrpc": "2.0", "id": 3, "method": "textDocument/completion",
     "params": {"textDocument": {"uri": "file:///vtest.mimi"},
                "position": {"line": 0, "character": 0}}},
    {"jsonrpc": "2.0", "id": 4, "method": "textDocument/rename",
     "params": {"textDocument": {"uri": "file:///vtest.mimi"},
                "position": {"line": 2, "character": 8},
                "newName": "person"}},
    {"jsonrpc": "2.0", "id": 5, "method": "shutdown", "params": None},
    {"jsonrpc": "2.0", "method": "exit", "params": {}},
]

input_data = "".join(encode_msg(r) for r in requests)
proc = subprocess.Popen([MIMI, "lsp"], stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
stdout, _ = proc.communicate(input=input_data.encode(), timeout=10)
output = stdout.decode()

print("=== LSP Responses ===")
lines = output.split("\n")
for l in lines:
    if l.strip():
        print(l[:200])

print("\n=== Validation ===")
checks = [
    ('"id":1', "Initialize"),
    ('"id":2', "Hover"),
    ('"id":3', "Completion"),
    ('"id":4', "Rename"),
    ('"id":5', "Shutdown"),
]
ok = True
for pat, label in checks:
    if pat in output:
        print(f"  ✅ {label}")
    else:
        print(f"  ❌ {label}")
        ok = False
sys.exit(0 if ok else 1)