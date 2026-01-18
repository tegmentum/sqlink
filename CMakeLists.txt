cmake_minimum_required(VERSION 3.16)

project(sqlite-wasm
    VERSION 0.1.0
    LANGUAGES C
    DESCRIPTION "SQLite compiled to WebAssembly Component"
)

set(CMAKE_C_STANDARD 11)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)

set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

# SQLite source (amalgamation)
set(SQLITE_SOURCE_DIR ${CMAKE_SOURCE_DIR}/deps/sqlite)

if(NOT EXISTS ${SQLITE_SOURCE_DIR}/sqlite3.c)
    message(FATAL_ERROR
        "SQLite source not found at ${SQLITE_SOURCE_DIR}\n"
        "Please run: ./scripts/download-sqlite.sh"
    )
endif()

# =============================================================================
# SQLite library
# =============================================================================

add_library(sqlite3 STATIC
    ${SQLITE_SOURCE_DIR}/sqlite3.c
)

target_include_directories(sqlite3 PUBLIC
    ${SQLITE_SOURCE_DIR}
)

# SQLite configuration for WASI
# Note: FTS5, RTREE, GEOPOLY, JSON1 are built as separate WASM extensions
target_compile_definitions(sqlite3 PRIVATE
    SQLITE_THREADSAFE=1
    SQLITE_MUTEX_NOOP
    SQLITE_ENABLE_MATH_FUNCTIONS
    SQLITE_DQS=0
    SQLITE_DEFAULT_MEMSTATUS=0
    SQLITE_DEFAULT_WAL_SYNCHRONOUS=1
    SQLITE_LIKE_DOESNT_MATCH_BLOBS
    SQLITE_MAX_EXPR_DEPTH=0
    SQLITE_USE_ALLOCA
    SQLITE_OMIT_DEPRECATED
    SQLITE_OMIT_SHARED_CACHE
)

# =============================================================================
# WASM bindings
# =============================================================================

add_library(sqlite_wasm_bindings STATIC
    src/bindings.c
)

target_include_directories(sqlite_wasm_bindings PUBLIC
    ${CMAKE_SOURCE_DIR}/src
    ${SQLITE_SOURCE_DIR}
)

target_link_libraries(sqlite_wasm_bindings PUBLIC sqlite3)

# =============================================================================
# WASM component
# =============================================================================

add_executable(sqlite.wasm src/main.c)

target_include_directories(sqlite.wasm PRIVATE ${SQLITE_SOURCE_DIR})
target_link_libraries(sqlite.wasm PRIVATE sqlite_wasm_bindings)

target_link_options(sqlite.wasm PRIVATE
    -mexec-model=reactor
    -Wl,--export-all
)

# =============================================================================
# Installation
# =============================================================================

install(TARGETS sqlite.wasm RUNTIME DESTINATION bin)
install(TARGETS sqlite3 ARCHIVE DESTINATION lib)
install(FILES ${SQLITE_SOURCE_DIR}/sqlite3.h DESTINATION include)
install(FILES wit/sqlite.wit DESTINATION share/wit)

message(STATUS "")
message(STATUS "sqlite-wasm Configuration Summary")
message(STATUS "==================================")
message(STATUS "Version: ${PROJECT_VERSION}")
message(STATUS "SQLite source: ${SQLITE_SOURCE_DIR}")
message(STATUS "")
