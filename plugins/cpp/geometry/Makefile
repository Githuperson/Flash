# Builds the C++ example plugin into build/ (Linux and macOS). Windows: see PLUGIN_ABI.md.
UNAME := $(shell uname -s)
CXX ?= c++
CXXFLAGS ?= -std=c++17 -O2 -Wall -Wextra -fPIC -fvisibility=hidden -I../../../include
ifeq ($(UNAME),Darwin)
  LIB := build/libgeometry.dylib
  LDFLAGS := -dynamiclib
else
  LIB := build/libgeometry.so
  LDFLAGS := -shared
endif

all: $(LIB)

$(LIB): geometry.cpp ../../../include/flash_plugin.h ../../../include/flash_plugin.hpp
	mkdir -p build
	$(CXX) $(CXXFLAGS) $(LDFLAGS) geometry.cpp -o $@

clean:
	rm -rf build

.PHONY: all clean
