#!/usr/bin/env bash
# Canonical Mimi developer environment bootstrapper.
#
# Ensures `LLVM_SYS_181_PREFIX` points to a usable LLVM 18 toolchain wrapper so
# Cargo commands do not fail after `/tmp` resets or on hosts where system
# `llvm-config` points to a newer LLVM major version.
#
# Usage:
#   source scripts/dev-env.sh                 # exports env vars in current shell
#   scripts/dev-env.sh cargo check --all-targets

_mimi_dev_env_main() {
    local root_dir wrapper_root
    root_dir="$(cd -P "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

    if [ -n "${LLVM_SYS_181_PREFIX:-}" ] && [ -x "${LLVM_SYS_181_PREFIX}/bin/llvm-config" ]; then
        export LLVM_SYS_181_PREFIX
    else
        wrapper_root="${MIMI_LLVM_WRAPPER:-/tmp/llvm-wrapper}"
        if [ ! -x "${wrapper_root}/bin/llvm-config" ] || [ ! -e "${wrapper_root}/lib/libLLVM.so.18.1" ]; then
            echo "[dev-env] Initializing LLVM 18 wrapper at ${wrapper_root}..." >&2
            MIMI_LLVM_WRAPPER="$wrapper_root" bash "${root_dir}/scripts/setup-llvm-wrapper.sh" >&2
        fi
        export LLVM_SYS_181_PREFIX="$wrapper_root"
    fi

    # Linking many LLVM/Z3 test targets in parallel (~2.4 GiB RSS per rust-lld)
    # can exhaust host RAM and trigger swap thrashing unless bounded.
    if [ -z "${CARGO_BUILD_JOBS:-}" ] && [ -r /proc/meminfo ]; then
        local mem_kb mem_gb max_link_jobs nproc_count
        mem_kb="$(awk '/^MemTotal:/ {print $2; exit}' /proc/meminfo 2>/dev/null || echo 0)"
        if [ "${mem_kb:-0}" -gt 0 ] 2>/dev/null; then
            mem_gb=$((mem_kb / 1024 / 1024))
            max_link_jobs=$((mem_gb / 8))
            [ "$max_link_jobs" -lt 2 ] && max_link_jobs=2
            nproc_count="$(nproc 2>/dev/null || echo "$max_link_jobs")"
            if [ "$max_link_jobs" -lt "$nproc_count" ]; then
                export CARGO_BUILD_JOBS="$max_link_jobs"
            fi
        fi
    fi

    export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
}

_mimi_dev_env_main

if [ "${BASH_SOURCE[0]}" = "$0" ] && [ "$#" -gt 0 ]; then
    exec "$@"
fi
