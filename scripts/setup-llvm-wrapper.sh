#!/bin/bash
# Create a truthful LLVM 18 host-only configuration for llvm-sys.
#
# The old wrapper mixed LLVM 23 headers with an LLVM 18 shared library and
# relied on --unresolved-symbols plus an untracked LD_PRELOAD shim.  That is
# not a compiler configuration: it can hide an ABI mismatch and it cannot be
# used as release evidence.  This wrapper exposes only the real LLVM 18
# shared library.  The root Cargo feature `llvm18-host-dynamic` disables the
# llvm-sys all-target C wrapper, so no mismatched target declarations are
# compiled on hosts that have the LLVM 18 runtime but not its dev headers.
set -euo pipefail

LLVM_ROOT="${MIMI_LLVM18_ROOT:-/usr/lib/llvm-18}"
LLVM_LIBDIR="${MIMI_LLVM18_LIBDIR:-${LLVM_ROOT}/lib}"
LLVM_SHARED="${MIMI_LLVM18_SHARED:-${LLVM_LIBDIR}/libLLVM.so.18.1}"
WRAPPER_ROOT="${MIMI_LLVM_WRAPPER:-/tmp/llvm-wrapper}"

if [ ! -r "$LLVM_SHARED" ]; then
    echo "LLVM 18 shared library not found: $LLVM_SHARED" >&2
    exit 1
fi

REAL_CONFIG="${MIMI_LLVM18_CONFIG:-}"
if [ -z "$REAL_CONFIG" ]; then
    for candidate in /usr/bin/llvm-config-18 "${LLVM_ROOT}/bin/llvm-config"; do
        if [ -x "$candidate" ]; then
            REAL_CONFIG="$candidate"
            break
        fi
    done
fi

if [ -n "$REAL_CONFIG" ]; then
    LLVM_VERSION="$($REAL_CONFIG --version)"
else
    # A runtime-only installation has no llvm-config.  Read the package
    # version when available; otherwise refuse to invent a version string.
    LLVM_VERSION="$(dpkg-query -W -f='${Version}' libllvm18 2>/dev/null || true)"
    LLVM_VERSION="${LLVM_VERSION#*:}"
    LLVM_VERSION="${LLVM_VERSION%%-*}"
    if [ -z "$LLVM_VERSION" ]; then
        echo "llvm-config-18 or an authoritative libllvm18 package version is required" >&2
        exit 1
    fi
fi

mkdir -p "${WRAPPER_ROOT}/bin" "${WRAPPER_ROOT}/lib"
ln -sfn "$LLVM_SHARED" "${WRAPPER_ROOT}/lib/libLLVM.so.18.1"
ln -sfn "libLLVM.so.18.1" "${WRAPPER_ROOT}/lib/libLLVM.so.18"
ln -sfn "libLLVM.so.18.1" "${WRAPPER_ROOT}/lib/libLLVM.so"

if [ -n "$REAL_CONFIG" ]; then
    cat > "${WRAPPER_ROOT}/llvm-config" <<EOF
#!/bin/bash
exec "$REAL_CONFIG" "\$@"
EOF
else
    cat > "${WRAPPER_ROOT}/llvm-config" <<EOF
#!/bin/bash
set -euo pipefail
case "\${1:-}" in
  --version) echo "$LLVM_VERSION" ;;
  --prefix) echo "$LLVM_ROOT" ;;
  --libdir) echo "$WRAPPER_ROOT/lib" ;;
  --includedir) echo "$LLVM_ROOT/include" ;;
  --cflags) echo "" ;;
  --ldflags) echo "-L$WRAPPER_ROOT/lib" ;;
  --libs) echo "-lLLVM" ;;
  --system-libs) echo "-ldl -lpthread -lm" ;;
  --libnames) echo "libLLVM.so" ;;
  *) echo "unsupported llvm-config query in host-only runtime wrapper: \${1:-}" >&2; exit 2 ;;
esac
EOF
fi
chmod +x "${WRAPPER_ROOT}/llvm-config"
ln -sfn "${WRAPPER_ROOT}/llvm-config" "${WRAPPER_ROOT}/bin/llvm-config"

echo "LLVM wrapper ready: $(${WRAPPER_ROOT}/llvm-config --version)"
echo "LLVM shared library: $LLVM_SHARED"
echo "Cargo feature: llvm18-host-dynamic"
