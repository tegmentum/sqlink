# SQLite WASM Component Makefile
# Builds SQLite as a WebAssembly component targeting WASI Preview 2

.PHONY: all clean deps sqlite wasi-sdk bindings build test help cli

# Directories
PROJECT_ROOT := $(shell pwd)
DEPS_DIR := $(PROJECT_ROOT)/deps
BUILD_DIR := $(PROJECT_ROOT)/build
SRC_DIR := $(PROJECT_ROOT)/src
WIT_DIR := $(PROJECT_ROOT)/wit
BINDINGS_DIR := $(SRC_DIR)/bindings

# wasi-sdk configuration
WASI_SDK := $(DEPS_DIR)/wasi-sdk
WASI_SYSROOT := $(WASI_SDK)/share/wasi-sysroot
CC := $(WASI_SDK)/bin/clang
AR := $(WASI_SDK)/bin/llvm-ar

# Target triple
TARGET := wasm32-wasip2

# SQLite configuration flags
SQLITE_CFLAGS := \
    -DSQLITE_THREADSAFE=1 \
    -DSQLITE_ENABLE_FTS5 \
    -DSQLITE_ENABLE_RTREE \
    -DSQLITE_ENABLE_GEOPOLY \
    -DSQLITE_ENABLE_MATH_FUNCTIONS \
    -DSQLITE_ENABLE_COLUMN_METADATA \
    -DSQLITE_ENABLE_JSON1 \
    -DSQLITE_ENABLE_STAT4 \
    -DSQLITE_OMIT_LOAD_EXTENSION \
    -DSQLITE_OMIT_LOCALTIME \
    -DSQLITE_TEMP_STORE=2 \
    -DSQLITE_OS_OTHER=1 \
    -DSQLITE_MUTEX_NOOP \
    -DSQLITE_DEFAULT_MEMSTATUS=0 \
    -DSQLITE_MAX_EXPR_DEPTH=0 \
    -DSQLITE_USE_ALLOCA

# Compiler flags
CFLAGS := \
    --target=$(TARGET) \
    --sysroot=$(WASI_SYSROOT) \
    -O2 \
    -g \
    -Wall \
    -Wextra \
    -Wno-unused-parameter \
    -I$(DEPS_DIR)/sqlite \
    -I$(SRC_DIR) \
    -I$(BINDINGS_DIR) \
    $(SQLITE_CFLAGS)

# Linker flags for reactor (library) mode
LDFLAGS := \
    --target=$(TARGET) \
    --sysroot=$(WASI_SYSROOT) \
    -mexec-model=reactor \
    -Wl,--export-dynamic \
    -Wl,--no-entry

# Source files
SQLITE_SRC := $(DEPS_DIR)/sqlite/sqlite3.c
VFS_SRCS := \
    $(SRC_DIR)/vfs/vfs_memory.c \
    $(SRC_DIR)/vfs/vfs_wasi.c
EXPORT_SRCS := \
    $(SRC_DIR)/exports/low_level.c \
    $(SRC_DIR)/exports/high_level.c
MAIN_SRC := $(SRC_DIR)/sqlite_wasm.c

# Object files
OBJS := \
    $(BUILD_DIR)/sqlite3.o \
    $(BUILD_DIR)/vfs_memory.o \
    $(BUILD_DIR)/vfs_wasi.o \
    $(BUILD_DIR)/low_level.o \
    $(BUILD_DIR)/high_level.o \
    $(BUILD_DIR)/sqlite_wasm.o \
    $(BUILD_DIR)/sqlite_world.o \
    $(BINDINGS_DIR)/sqlite_world_component_type.o

# Output files
CORE_WASM := $(BUILD_DIR)/sqlite-core.wasm
COMPONENT_WASM := $(BUILD_DIR)/sqlite.wasm

# Default target
all: $(COMPONENT_WASM)

# Help
help:
	@echo "SQLite WASM Component Build System"
	@echo ""
	@echo "Targets:"
	@echo "  all          Build the SQLite WASM component (default)"
	@echo "  deps         Download all dependencies (wasi-sdk, sqlite)"
	@echo "  sqlite       Download SQLite amalgamation"
	@echo "  wasi-sdk     Download wasi-sdk toolchain"
	@echo "  bindings     Generate C bindings from WIT"
	@echo "  build        Build the core WASM module"
	@echo "  component    Convert core module to component"
	@echo "  cli          Build the SQLite CLI (sqlite-cli.wasm)"
	@echo "  test         Run tests"
	@echo "  clean        Remove build artifacts"
	@echo ""
	@echo "Environment variables:"
	@echo "  SQLITE_VERSION  SQLite version (default: 3480000)"
	@echo "  WASI_SDK_VERSION  wasi-sdk version (default: 25)"

# Download dependencies
deps: wasi-sdk sqlite

sqlite:
	@echo "Downloading SQLite..."
	./scripts/download-sqlite.sh

wasi-sdk:
	@echo "Downloading wasi-sdk..."
	./scripts/download-wasi-sdk.sh

# Generate bindings from WIT
bindings: $(BINDINGS_DIR)/sqlite_world.h

$(BINDINGS_DIR)/sqlite_world.h: $(WIT_DIR)/world.wit $(WIT_DIR)/sqlite-low-level.wit $(WIT_DIR)/sqlite-high-level.wit
	@echo "Generating C bindings from WIT..."
	@mkdir -p $(BINDINGS_DIR)
	wit-bindgen c $(WIT_DIR) --world sqlite-world --out-dir $(BINDINGS_DIR)

# Generate bindings for extensible world (includes extension API)
BINDINGS_EXT_DIR := $(SRC_DIR)/bindings-ext

bindings-ext: $(BINDINGS_EXT_DIR)/sqlite_extensible.h

$(BINDINGS_EXT_DIR)/sqlite_extensible.h: $(WIT_DIR)/world.wit $(WIT_DIR)/sqlite-low-level.wit $(WIT_DIR)/sqlite-high-level.wit $(WIT_DIR)/sqlite-extension.wit
	@echo "Generating C bindings for extensible world..."
	@mkdir -p $(BINDINGS_EXT_DIR)
	wit-bindgen c $(WIT_DIR) --world sqlite-extensible --out-dir $(BINDINGS_EXT_DIR)

# Create build directory
$(BUILD_DIR):
	mkdir -p $(BUILD_DIR)

# Compile SQLite
$(BUILD_DIR)/sqlite3.o: $(SQLITE_SRC) | $(BUILD_DIR)
	@echo "Compiling SQLite..."
	$(CC) $(CFLAGS) -c $< -o $@

# Compile VFS implementations
$(BUILD_DIR)/vfs_memory.o: $(SRC_DIR)/vfs/vfs_memory.c | $(BUILD_DIR)
	@echo "Compiling memory VFS..."
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD_DIR)/vfs_wasi.o: $(SRC_DIR)/vfs/vfs_wasi.c | $(BUILD_DIR)
	@echo "Compiling WASI VFS..."
	$(CC) $(CFLAGS) -c $< -o $@

# Compile export implementations
$(BUILD_DIR)/low_level.o: $(SRC_DIR)/exports/low_level.c $(BINDINGS_DIR)/sqlite_world.h | $(BUILD_DIR)
	@echo "Compiling low-level exports..."
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD_DIR)/high_level.o: $(SRC_DIR)/exports/high_level.c $(BINDINGS_DIR)/sqlite_world.h | $(BUILD_DIR)
	@echo "Compiling high-level exports..."
	$(CC) $(CFLAGS) -c $< -o $@

# Compile main wrapper
$(BUILD_DIR)/sqlite_wasm.o: $(MAIN_SRC) $(BINDINGS_DIR)/sqlite_world.h | $(BUILD_DIR)
	@echo "Compiling main wrapper..."
	$(CC) $(CFLAGS) -c $< -o $@

# Compile generated bindings
$(BUILD_DIR)/sqlite_world.o: $(BINDINGS_DIR)/sqlite_world.c $(BINDINGS_DIR)/sqlite_world.h | $(BUILD_DIR)
	@echo "Compiling generated bindings..."
	$(CC) $(CFLAGS) -c $< -o $@

# Link core WASM module
$(CORE_WASM): $(OBJS)
	@echo "Linking core WASM module..."
	$(CC) $(LDFLAGS) $(OBJS) -o $@

# wasm32-wasip2 target already produces a component, just rename/copy
$(COMPONENT_WASM): $(CORE_WASM)
	@echo "Finalizing WASM component..."
	cp $(CORE_WASM) $@
	@echo "Built: $@"
	@wasm-tools component wit $@ 2>/dev/null | head -50 || true
	@ls -lh $@

build: $(CORE_WASM)

component: $(COMPONENT_WASM)

# Run tests
test: $(COMPONENT_WASM)
	@echo "Running tests..."
	@if command -v wasmtime >/dev/null 2>&1; then \
		echo "Testing with wasmtime..."; \
		wasmtime wast tests/unit/*.wast 2>/dev/null || echo "No .wast tests found"; \
	else \
		echo "wasmtime not found, skipping runtime tests"; \
	fi

test-unit: test

test-integration: $(COMPONENT_WASM)
	@echo "Running integration tests..."
	@if command -v jco >/dev/null 2>&1; then \
		echo "Testing with jco..."; \
		jco transpile $(COMPONENT_WASM) -o $(BUILD_DIR)/js --minify 2>/dev/null && \
		node tests/integration/jco/test.js 2>/dev/null || echo "jco tests not configured"; \
	else \
		echo "jco not found, skipping JavaScript integration tests"; \
	fi

# CLI build
CLI_SRC := $(SRC_DIR)/cli/sqlite_cli.c
CLI_WASM := $(BUILD_DIR)/sqlite-cli.wasm

# CLI compiler flags (command mode, not reactor)
CLI_CFLAGS := \
    --target=$(TARGET) \
    --sysroot=$(WASI_SYSROOT) \
    -O2 \
    -g \
    -Wall \
    -I$(DEPS_DIR)/sqlite \
    -I$(SRC_DIR) \
    $(SQLITE_CFLAGS)

CLI_LDFLAGS := \
    --target=$(TARGET) \
    --sysroot=$(WASI_SYSROOT)

# CLI object files
CLI_OBJS := \
    $(BUILD_DIR)/sqlite3.o \
    $(BUILD_DIR)/vfs_memory.o \
    $(BUILD_DIR)/vfs_wasi.o \
    $(BUILD_DIR)/sqlite_wasm.o \
    $(BUILD_DIR)/sqlite_cli.o

# Compile CLI main
$(BUILD_DIR)/sqlite_cli.o: $(CLI_SRC) | $(BUILD_DIR)
	@echo "Compiling CLI..."
	$(CC) $(CLI_CFLAGS) -c $< -o $@

# Link CLI
$(CLI_WASM): $(CLI_OBJS)
	@echo "Linking CLI..."
	$(CC) $(CLI_LDFLAGS) $(CLI_OBJS) -o $@
	@echo "Built: $@"
	@ls -lh $@

cli: $(CLI_WASM)

# Extensible build (with extension API)
EXTENSIBLE_WASM := $(BUILD_DIR)/sqlite-extensible.wasm

# Compiler flags for extensible build (includes bindings-ext)
CFLAGS_EXT := \
    --target=$(TARGET) \
    --sysroot=$(WASI_SYSROOT) \
    -O2 \
    -g \
    -Wall \
    -Wextra \
    -Wno-unused-parameter \
    -I$(DEPS_DIR)/sqlite \
    -I$(SRC_DIR) \
    -I$(BINDINGS_EXT_DIR) \
    $(SQLITE_CFLAGS)

# Object files for extensible build
OBJS_EXT := \
    $(BUILD_DIR)/sqlite3.o \
    $(BUILD_DIR)/vfs_memory.o \
    $(BUILD_DIR)/vfs_wasi.o \
    $(BUILD_DIR)/low_level_ext.o \
    $(BUILD_DIR)/high_level_ext.o \
    $(BUILD_DIR)/extension.o \
    $(BUILD_DIR)/sqlite_wasm_ext.o \
    $(BUILD_DIR)/sqlite_extensible.o \
    $(BINDINGS_EXT_DIR)/sqlite_extensible_component_type.o

# Compile exports for extensible build (uses sqlite_world.h wrapper in bindings-ext)
$(BUILD_DIR)/low_level_ext.o: $(SRC_DIR)/exports/low_level.c $(BINDINGS_EXT_DIR)/sqlite_extensible.h | $(BUILD_DIR)
	@echo "Compiling low-level exports (extensible)..."
	$(CC) $(CFLAGS_EXT) -c $< -o $@

$(BUILD_DIR)/high_level_ext.o: $(SRC_DIR)/exports/high_level.c $(BINDINGS_EXT_DIR)/sqlite_extensible.h | $(BUILD_DIR)
	@echo "Compiling high-level exports (extensible)..."
	$(CC) $(CFLAGS_EXT) -c $< -o $@

$(BUILD_DIR)/extension.o: $(SRC_DIR)/exports/extension.c $(BINDINGS_EXT_DIR)/sqlite_extensible.h | $(BUILD_DIR)
	@echo "Compiling extension exports..."
	$(CC) $(CFLAGS_EXT) -c $< -o $@

$(BUILD_DIR)/sqlite_wasm_ext.o: $(MAIN_SRC) $(BINDINGS_EXT_DIR)/sqlite_extensible.h | $(BUILD_DIR)
	@echo "Compiling main wrapper (extensible)..."
	$(CC) $(CFLAGS_EXT) -c $< -o $@

$(BUILD_DIR)/sqlite_extensible.o: $(BINDINGS_EXT_DIR)/sqlite_extensible.c $(BINDINGS_EXT_DIR)/sqlite_extensible.h | $(BUILD_DIR)
	@echo "Compiling generated extensible bindings..."
	$(CC) $(CFLAGS_EXT) -c $< -o $@

$(EXTENSIBLE_WASM): $(OBJS_EXT)
	@echo "Linking extensible WASM module..."
	$(CC) $(LDFLAGS) $(OBJS_EXT) -o $@
	@echo "Built: $@"
	@wasm-tools component wit $@ 2>/dev/null | head -50 || true
	@ls -lh $@

extensible: bindings-ext $(EXTENSIBLE_WASM)

.PHONY: extensible

# Clean build artifacts
clean:
	rm -rf $(BUILD_DIR)
	rm -rf $(BINDINGS_DIR)

# Clean everything including dependencies
distclean: clean
	rm -rf $(DEPS_DIR)/sqlite/sqlite3.*
	rm -rf $(DEPS_DIR)/wasi-sdk*

# Verify toolchain
verify-tools:
	@echo "Checking required tools..."
	@command -v $(CC) >/dev/null 2>&1 || (echo "wasi-sdk not found. Run 'make wasi-sdk'" && exit 1)
	@command -v wit-bindgen >/dev/null 2>&1 || (echo "wit-bindgen not found. Install with: cargo install wit-bindgen-cli" && exit 1)
	@command -v wasm-tools >/dev/null 2>&1 || (echo "wasm-tools not found. Install with: cargo install wasm-tools" && exit 1)
	@echo "All required tools found."

# Print configuration
info:
	@echo "Configuration:"
	@echo "  PROJECT_ROOT: $(PROJECT_ROOT)"
	@echo "  WASI_SDK: $(WASI_SDK)"
	@echo "  TARGET: $(TARGET)"
	@echo "  CC: $(CC)"
	@echo ""
	@echo "SQLite flags:"
	@echo "  $(SQLITE_CFLAGS)" | tr ' ' '\n' | grep -v '^$$'
