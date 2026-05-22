# WASI SDK CMake Toolchain for WASI Preview 2
#
# This toolchain configures CMake for cross-compilation to WebAssembly
# using the WASI SDK targeting WASI Preview 2 (wasip2).

if(NOT DEFINED WASI_SDK_PREFIX)
    if(DEFINED ENV{WASI_SDK_PREFIX})
        set(WASI_SDK_PREFIX "$ENV{WASI_SDK_PREFIX}")
    elseif(EXISTS "$ENV{HOME}/wasi-sdk-33")
        set(WASI_SDK_PREFIX "$ENV{HOME}/wasi-sdk-33")
    elseif(EXISTS "$ENV{HOME}/wasi-sdk")
        set(WASI_SDK_PREFIX "$ENV{HOME}/wasi-sdk")
    elseif(EXISTS "/opt/wasi-sdk")
        set(WASI_SDK_PREFIX "/opt/wasi-sdk")
    else()
        message(FATAL_ERROR
            "WASI SDK not found. Please set WASI_SDK_PREFIX environment variable.\n"
            "Download from: https://github.com/WebAssembly/wasi-sdk/releases"
        )
    endif()
endif()

message(STATUS "Using WASI SDK: ${WASI_SDK_PREFIX}")

# Include the official wasi-sdk toolchain if available
if(EXISTS "${WASI_SDK_PREFIX}/share/cmake/wasi-sdk-p2.cmake")
    include("${WASI_SDK_PREFIX}/share/cmake/wasi-sdk-p2.cmake")
else()
    # Manual configuration for older wasi-sdk versions
    set(CMAKE_SYSTEM_NAME WASI)
    set(CMAKE_SYSTEM_VERSION 1)
    set(CMAKE_SYSTEM_PROCESSOR wasm32)

    set(WASI_TARGET "wasm32-wasip2")

    set(CMAKE_C_COMPILER "${WASI_SDK_PREFIX}/bin/clang")
    set(CMAKE_CXX_COMPILER "${WASI_SDK_PREFIX}/bin/clang++")
    set(CMAKE_AR "${WASI_SDK_PREFIX}/bin/llvm-ar")
    set(CMAKE_RANLIB "${WASI_SDK_PREFIX}/bin/llvm-ranlib")

    set(CMAKE_SYSROOT "${WASI_SDK_PREFIX}/share/wasi-sysroot")

    set(CMAKE_C_FLAGS_INIT "--target=${WASI_TARGET} --sysroot=${CMAKE_SYSROOT}")
    set(CMAKE_CXX_FLAGS_INIT "--target=${WASI_TARGET} --sysroot=${CMAKE_SYSROOT}")

    set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
    set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
    set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
    set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
endif()

# Static libraries only
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)

# Reactor execution model
set(CMAKE_EXE_LINKER_FLAGS "${CMAKE_EXE_LINKER_FLAGS} -mexec-model=reactor" CACHE STRING "" FORCE)

# Installation directories
set(CMAKE_INSTALL_LIBDIR "lib" CACHE PATH "" FORCE)
set(CMAKE_INSTALL_INCLUDEDIR "include" CACHE PATH "" FORCE)
set(CMAKE_INSTALL_BINDIR "bin" CACHE PATH "" FORCE)
