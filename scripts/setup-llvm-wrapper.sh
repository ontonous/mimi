#!/bin/bash
# Recreate the LLVM wrapper for llvm-sys (avoids libpolly-18-dev dependency)
set -e
mkdir -p /tmp/llvm-wrapper/{bin,lib}

cat > /tmp/llvm-wrapper/llvm-config << 'EOF'
#!/bin/bash
if [ "$1" = "--libdir" ]; then
    echo "/tmp/llvm-wrapper/lib"
else
    exec /usr/bin/llvm-config-18 "$@"
fi
EOF
chmod +x /tmp/llvm-wrapper/llvm-config
ln -sf /tmp/llvm-wrapper/llvm-config /tmp/llvm-wrapper/bin/llvm-config

# Symlink all LLVM static/shared libs into wrapper
cd /tmp/llvm-wrapper/lib
ln -sf /usr/lib/llvm-18/lib/libLLVM*.a . 2>/dev/null
ln -sf /usr/lib/llvm-18/lib/libLLVM*.so* . 2>/dev/null

echo "" > /tmp/_empty.o
ar rcs /tmp/llvm-wrapper/lib/libPolly.a /tmp/_empty.o
ar rcs /tmp/llvm-wrapper/lib/libPollyISL.a /tmp/_empty.o
rm -f /tmp/_empty.o

echo "LLVM wrapper ready: $(/tmp/llvm-wrapper/llvm-config --version)"
