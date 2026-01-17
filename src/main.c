/**
 * SQLite WASM Main Entry Point
 */

#include <stdint.h>
#include "sqlite3.h"

extern const char* sqlite_wasm_version(void);

__attribute__((export_name("sqlite_version")))
const char* sqlite_version_export(void) {
    return sqlite_wasm_version();
}

__attribute__((export_name("info_version")))
const char* info_version(void) {
    return sqlite3_libversion();
}

__attribute__((export_name("info_version_number")))
int32_t info_version_number(void) {
    return sqlite3_libversion_number();
}
