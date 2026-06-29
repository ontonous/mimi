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

all: libxmath.so bindings $(RUNTIME_SHIM_SO) c_main rust_main xmath$(EXT_SUFFIX)

libxmath.so: xmath.mimi
	$(MIMI) build --shared $< -o $@

bindings: xmath.mimi
	$(MIMI) bindgen $< -o bindings
	ln -sf bindings/xmath.h mimi_ffi.h

$(RUNTIME_SHIM_SO): $(RUNTIME_SHIM_DIR)/Cargo.toml $(RUNTIME_SHIM_DIR)/src/lib.rs
	$(CARGO) build --manifest-path $(RUNTIME_SHIM_DIR)/Cargo.toml

c_main: c_main.c libxmath.so bindings $(RUNTIME_SHIM_SO)
	$(CC) -o $@ c_main.c -I. -L. -L$(RUNTIME_SHIM_DIR)/target/debug -lxmath -lmimi_runtime_shim -Wl,-rpath,'$$ORIGIN' -Wl,-rpath,'$$ORIGIN/$(RUNTIME_SHIM_DIR)/target/debug'

rust_main: rust_main.rs libxmath.so bindings $(RUNTIME_SHIM_SO)
	rustc -L. -L$(RUNTIME_SHIM_DIR)/target/debug rust_main.rs -o $@ -lxmath -lmimi_runtime_shim -C link-arg=-Wl,-rpath,'$$ORIGIN' -C link-arg=-Wl,-rpath,'$$ORIGIN/$(RUNTIME_SHIM_DIR)/target/debug'

xmath$(EXT_SUFFIX): bindings/xmath_pybind.cpp libxmath.so bindings $(RUNTIME_SHIM_SO)
	$(CXX) -shared -fPIC -O2 -std=c++17 $$(python3 -m pybind11 --includes) $< -I. -L. -L$(RUNTIME_SHIM_DIR)/target/debug -lxmath -lmimi_runtime_shim -Wl,-rpath,'$$ORIGIN' -Wl,-rpath,'$$ORIGIN/$(RUNTIME_SHIM_DIR)/target/debug' -o $@

test-c: c_main
	./c_main

test-rust: rust_main
	./rust_main

test-python: xmath$(EXT_SUFFIX)
	$(PYTHON) python_test.py

test: test-c test-rust test-python

clean:
	rm -f libxmath.so c_main rust_main xmath*.so mimi_ffi.h
	rm -rf bindings
