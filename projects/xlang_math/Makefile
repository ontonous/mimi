MIMI = ../../target/debug/mimi
CARGO = cargo
CC = gcc
CXX = g++
PYTHON = python3
EXT_SUFFIX := $(shell $(PYTHON) -c "import importlib.machinery; print(importlib.machinery.EXTENSION_SUFFIXES[0])")

export LLVM_SYS_180_PREFIX = /tmp/llvm-wrapper

.PHONY: all bindings clean test test-c test-rust test-python

all: libxmath.so bindings runtime_shim/target/debug/libmimi_runtime_shim.so c_main rust_main xmath$(EXT_SUFFIX)

libxmath.so: xmath.mimi
	$(MIMI) build --shared $< -o $@

bindings: xmath.mimi
	$(MIMI) bindgen $< -o bindings
	ln -sf bindings/xmath.h mimi_ffi.h

runtime_shim/target/debug/libmimi_runtime_shim.so: runtime_shim/Cargo.toml runtime_shim/src/lib.rs
	$(CARGO) build --manifest-path runtime_shim/Cargo.toml
	cp $@ libmimi_runtime_shim.so

c_main: c_main.c libxmath.so bindings runtime_shim/target/debug/libmimi_runtime_shim.so
	$(CC) -o $@ c_main.c -I. -L. -lxmath -lmimi_runtime_shim -Wl,-rpath,'$$ORIGIN'

rust_main: rust_main.rs libxmath.so bindings runtime_shim/target/debug/libmimi_runtime_shim.so
	rustc -L. rust_main.rs -o $@ -lxmath -lmimi_runtime_shim -C link-arg=-Wl,-rpath,'$$ORIGIN'

xmath$(EXT_SUFFIX): bindings/xmath_pybind.cpp libxmath.so bindings runtime_shim/target/debug/libmimi_runtime_shim.so
	$(CXX) -shared -fPIC -O2 -std=c++17 $$(python3 -m pybind11 --includes) $< -I. -L. -lxmath -lmimi_runtime_shim -Wl,-rpath,'$$ORIGIN' -o $@

test-c: c_main
	./c_main

test-rust: rust_main
	./rust_main

test-python: xmath$(EXT_SUFFIX)
	$(PYTHON) python_test.py

test: test-c test-rust test-python

clean:
	rm -f libxmath.so libmimi_runtime_shim.so c_main rust_main xmath*.so mimi_ffi.h
	rm -rf bindings runtime_shim/target
