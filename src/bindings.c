/**
 * SQLite WASM Bindings
 */

#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include "sqlite3.h"

/* Version info */
const char* sqlite_wasm_version(void) {
    return sqlite3_libversion();
}

int32_t sqlite_wasm_version_number(void) {
    return sqlite3_libversion_number();
}

const char* sqlite_wasm_source_id(void) {
    return sqlite3_sourceid();
}

/* Database operations */
sqlite3* sqlite_wasm_open(const char* path, int* result) {
    sqlite3* db = NULL;
    *result = sqlite3_open(path, &db);
    return db;
}

sqlite3* sqlite_wasm_open_memory(int* result) {
    sqlite3* db = NULL;
    *result = sqlite3_open(":memory:", &db);
    return db;
}

int sqlite_wasm_close(sqlite3* db) {
    return sqlite3_close(db);
}

int sqlite_wasm_exec(sqlite3* db, const char* sql) {
    return sqlite3_exec(db, sql, NULL, NULL, NULL);
}

int64_t sqlite_wasm_last_insert_rowid(sqlite3* db) {
    return sqlite3_last_insert_rowid(db);
}

int sqlite_wasm_changes(sqlite3* db) {
    return sqlite3_changes(db);
}

int sqlite_wasm_total_changes(sqlite3* db) {
    return sqlite3_total_changes(db);
}

const char* sqlite_wasm_errmsg(sqlite3* db) {
    return sqlite3_errmsg(db);
}

/* Statement operations */
sqlite3_stmt* sqlite_wasm_prepare(sqlite3* db, const char* sql, int* result) {
    sqlite3_stmt* stmt = NULL;
    *result = sqlite3_prepare_v2(db, sql, -1, &stmt, NULL);
    return stmt;
}

int sqlite_wasm_bind_int(sqlite3_stmt* stmt, int index, int64_t value) {
    return sqlite3_bind_int64(stmt, index, value);
}

int sqlite_wasm_bind_double(sqlite3_stmt* stmt, int index, double value) {
    return sqlite3_bind_double(stmt, index, value);
}

int sqlite_wasm_bind_text(sqlite3_stmt* stmt, int index, const char* value) {
    return sqlite3_bind_text(stmt, index, value, -1, SQLITE_TRANSIENT);
}

int sqlite_wasm_bind_blob(sqlite3_stmt* stmt, int index, const void* value, int len) {
    return sqlite3_bind_blob(stmt, index, value, len, SQLITE_TRANSIENT);
}

int sqlite_wasm_bind_null(sqlite3_stmt* stmt, int index) {
    return sqlite3_bind_null(stmt, index);
}

int sqlite_wasm_step(sqlite3_stmt* stmt) {
    return sqlite3_step(stmt);
}

int sqlite_wasm_reset(sqlite3_stmt* stmt) {
    return sqlite3_reset(stmt);
}

int sqlite_wasm_clear_bindings(sqlite3_stmt* stmt) {
    return sqlite3_clear_bindings(stmt);
}

int sqlite_wasm_column_count(sqlite3_stmt* stmt) {
    return sqlite3_column_count(stmt);
}

const char* sqlite_wasm_column_name(sqlite3_stmt* stmt, int index) {
    return sqlite3_column_name(stmt, index);
}

int sqlite_wasm_column_type(sqlite3_stmt* stmt, int index) {
    return sqlite3_column_type(stmt, index);
}

int64_t sqlite_wasm_column_int(sqlite3_stmt* stmt, int index) {
    return sqlite3_column_int64(stmt, index);
}

double sqlite_wasm_column_double(sqlite3_stmt* stmt, int index) {
    return sqlite3_column_double(stmt, index);
}

const char* sqlite_wasm_column_text(sqlite3_stmt* stmt, int index) {
    return (const char*)sqlite3_column_text(stmt, index);
}

const void* sqlite_wasm_column_blob(sqlite3_stmt* stmt, int index) {
    return sqlite3_column_blob(stmt, index);
}

int sqlite_wasm_column_bytes(sqlite3_stmt* stmt, int index) {
    return sqlite3_column_bytes(stmt, index);
}

int sqlite_wasm_finalize(sqlite3_stmt* stmt) {
    return sqlite3_finalize(stmt);
}
