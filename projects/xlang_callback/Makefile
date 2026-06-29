MIMI = ../../target/debug/mimi
CARGO = cargo
CC = gcc
CXX = g++
PYTHON = python3
EXT_SUFFIX := $(shell $(PYTHON) -c "import importlib.machinery; print(importlib.machinery.EXTENSION_SUFFIXES[0])")

RUNTIME_SHIM_DIR = ../runtime_shim
RUNTIME_SHIM_SO = $(RUNTIME_SHIM_DIR)/target/debug/libmimi_runtime_shim.so

export LLVM_SYS_180_PREFIX = /tmp/llvm-wrapper

.PHONY: all bindings clean test test-c test-rust test-python

all: libxcallbacks.so bindings $(RUNTIME_SHIM_SO) c_main rust_main xcallbacks$(EXT_SUFFIX)

libxcallbacks.so: xcallbacks.mimi
	$(MIMI) build --shared $< -o $@

bindings: xcallbacks.mimi
	$(MIMI) bindgen $< -o bindings
	ln -sf bindings/xcallbacks.h mimi_ffi.h

$(RUNTIME_SHIM_SO): $(RUNTIME_SHIM_DIR)/Cargo.toml $(RUNTIME_SHIM_DIR)/src/lib.rs
	$(CARGO) build --manifest-path $(RUNTIME_SHIM_DIR)/Cargo.toml

c_main: c_main.c libxcallbacks.so bindings $(RUNTIME_SHIM_SO)
	$(CC) -o $@ c_main.c -I. -L. -L$(RUNTIME_SHIM_DIR)/target/debug -lxcallbacks -lmimi_runtime_shim -Wl,-rpath,'$$ORIGIN' -Wl,-rpath,'$$ORIGIN/$(RUNTIME_SHIM_DIR)/target/debug'

rust_main: rust_main.rs libxcallbacks.so bindings $(RUNTIME_SHIM_SO)
	rustc -L. -L$(RUNTIME_SHIM_DIR)/target/debug rust_main.rs -o $@ -lxcallbacks -lmimi_runtime_shim -C link-arg=-Wl,-rpath,'$$ORIGIN' -C link-arg=-Wl,-rpath,'$$ORIGIN/$(RUNTIME_SHIM_DIR)/target/debug'

xcallbacks$(EXT_SUFFIX): bindings/xcallbacks_pybind.cpp libxcallbacks.so bindings $(RUNTIME_SHIM_SO)
	$(CXX) -shared -fPIC -O2 -std=c++17 $$(python3 -m pybind11 --includes) $< -I. -L. -L$(RUNTIME_SHIM_DIR)/target/debug -lxcallbacks -lmimi_runtime_shim -Wl,-rpath,'$$ORIGIN' -Wl,-rpath,'$$ORIGIN/$(RUNTIME_SHIM_DIR)/target/debug' -o $@

test-c: c_main
	./c_main

test-rust: rust_main
	./rust_main

test-python: xcallbacks$(EXT_SUFFIX)
	$(PYTHON) python_test.py

test: test-c test-rust test-python

clean:
	rm -f libxcallbacks.so c_main rust_main xcallbacks*.so mimi_ffi.h
	rm -rf bindings
