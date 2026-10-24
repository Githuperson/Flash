# Builds the static libraries used by main.flash into build/ (Linux and macOS).
CC ?= cc
CXX ?= c++
AR ?= ar

all: build/libmathlib.a build/libstats.a build/librust_static.a

build/libmathlib.a: mathlib.c
	mkdir -p build
	$(CC) -O2 -fPIC -c mathlib.c -o build/mathlib.o
	$(AR) rcs $@ build/mathlib.o

build/libstats.a: stats.cpp
	mkdir -p build
	$(CXX) -std=c++17 -O2 -fPIC -c stats.cpp -o build/stats.o
	$(AR) rcs $@ build/stats.o

build/librust_static.a: rust_static.rs
	mkdir -p build
	rustc --crate-type staticlib -C panic=abort -C opt-level=2 --edition 2021 rust_static.rs -o $@

clean:
	rm -rf build

.PHONY: all clean
