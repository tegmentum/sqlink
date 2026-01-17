/*
 * SQLite WASM Low-Level API Implementation
 *
 * This file provides the implementation of the low-level C-style API
 * matching the wit-bindgen generated function signatures.
 */

#include <stdlib.h>
#include <string.h>
#include "sqlite3.h"
#include "sqlite_world.h"

/*
 * Handle management - direct pointer cast for wasm32
 */
#define PTR_TO_HANDLE(ptr) ((uint64_t)(uintptr_t)(ptr))
#define HANDLE_TO_DB(h) ((sqlite3*)(uintptr_t)(h))
#define HANDLE_TO_STMT(h) ((sqlite3_stmt*)(uintptr_t)(h))

/*
 * Result code conversion from SQLite to WIT enum
 */
static exports_sqlite_wasm_low_level_result_code_t sqlite_to_wit_result(int rc) {
    switch (rc) {
        case SQLITE_OK:         return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_OK;
        case SQLITE_ERROR:      return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_ERROR;
        case SQLITE_INTERNAL:   return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_INTERNAL;
        case SQLITE_PERM:       return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_PERM;
        case SQLITE_ABORT:      return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_ABORT;
        case SQLITE_BUSY:       return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_BUSY;
        case SQLITE_LOCKED:     return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_LOCKED;
        case SQLITE_NOMEM:      return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_NOMEM;
        case SQLITE_READONLY:   return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_READONLY;
        case SQLITE_INTERRUPT:  return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_INTERRUPT;
        case SQLITE_IOERR:      return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_IOERR;
        case SQLITE_CORRUPT:    return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_CORRUPT;
        case SQLITE_NOTFOUND:   return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_NOTFOUND;
        case SQLITE_FULL:       return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_FULL;
        case SQLITE_CANTOPEN:   return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_CANTOPEN;
        case SQLITE_PROTOCOL:   return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_PROTOCOL;
        case SQLITE_EMPTY:      return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_EMPTY;
        case SQLITE_SCHEMA:     return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_SCHEMA;
        case SQLITE_TOOBIG:     return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_TOOBIG;
        case SQLITE_CONSTRAINT: return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_CONSTRAINT;
        case SQLITE_MISMATCH:   return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_MISMATCH;
        case SQLITE_MISUSE:     return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_MISUSE;
        case SQLITE_NOLFS:      return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_NOLFS;
        case SQLITE_AUTH:       return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_AUTH;
        case SQLITE_FORMAT:     return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_FORMAT;
        case SQLITE_RANGE:      return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_RANGE;
        case SQLITE_NOTADB:     return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_NOTADB;
        case SQLITE_NOTICE:     return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_NOTICE;
        case SQLITE_WARNING:    return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_WARNING;
        case SQLITE_ROW:        return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_ROW;
        case SQLITE_DONE:       return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_DONE;
        default:                return EXPORTS_SQLITE_WASM_LOW_LEVEL_RESULT_CODE_ERROR;
    }
}

/*
 * Column type conversion
 */
static exports_sqlite_wasm_low_level_column_type_t sqlite_to_wit_column_type(int type) {
    switch (type) {
        case SQLITE_INTEGER:    return EXPORTS_SQLITE_WASM_LOW_LEVEL_COLUMN_TYPE_INTEGER;
        case SQLITE_FLOAT:      return EXPORTS_SQLITE_WASM_LOW_LEVEL_COLUMN_TYPE_FLOAT;
        case SQLITE_TEXT:       return EXPORTS_SQLITE_WASM_LOW_LEVEL_COLUMN_TYPE_TEXT;
        case SQLITE_BLOB:       return EXPORTS_SQLITE_WASM_LOW_LEVEL_COLUMN_TYPE_BLOB;
        case SQLITE_NULL:
        default:                return EXPORTS_SQLITE_WASM_LOW_LEVEL_COLUMN_TYPE_NULL;
    }
}

/*
 * Open flags conversion
 */
static int wit_to_sqlite_open_flags(exports_sqlite_wasm_low_level_open_flags_t wit_flags) {
    int flags = 0;

    if (wit_flags & EXPORTS_SQLITE_WASM_LOW_LEVEL_OPEN_FLAGS_READONLY)  flags |= SQLITE_OPEN_READONLY;
    if (wit_flags & EXPORTS_SQLITE_WASM_LOW_LEVEL_OPEN_FLAGS_READWRITE) flags |= SQLITE_OPEN_READWRITE;
    if (wit_flags & EXPORTS_SQLITE_WASM_LOW_LEVEL_OPEN_FLAGS_CREATE)    flags |= SQLITE_OPEN_CREATE;
    if (wit_flags & EXPORTS_SQLITE_WASM_LOW_LEVEL_OPEN_FLAGS_MEMORY)    flags |= SQLITE_OPEN_MEMORY;
    if (wit_flags & EXPORTS_SQLITE_WASM_LOW_LEVEL_OPEN_FLAGS_URI)       flags |= SQLITE_OPEN_URI;

    /* Default to readwrite if no mode specified */
    if (!(flags & (SQLITE_OPEN_READONLY | SQLITE_OPEN_READWRITE))) {
        flags |= SQLITE_OPEN_READWRITE;
    }

    return flags;
}

/* Helper to create a null-terminated string from sqlite_world_string_t */
static char *string_to_cstr(sqlite_world_string_t *str) {
    if (!str || !str->ptr || str->len == 0) {
        return NULL;
    }
    char *cstr = (char *)malloc(str->len + 1);
    if (cstr) {
        memcpy(cstr, str->ptr, str->len);
        cstr[str->len] = '\0';
    }
    return cstr;
}

/*
 * Low-level API exports
 */

bool exports_sqlite_wasm_low_level_open(
    sqlite_world_string_t *filename,
    exports_sqlite_wasm_low_level_open_flags_t open_flags,
    exports_sqlite_wasm_low_level_db_handle_t *ret,
    exports_sqlite_wasm_low_level_result_code_t *err
) {
    sqlite3 *db = NULL;
    char *fname = string_to_cstr(filename);

    int sqlite_flags = wit_to_sqlite_open_flags(open_flags);
    int rc = sqlite3_open_v2(fname ? fname : ":memory:", &db, sqlite_flags, NULL);

    if (fname) free(fname);

    if (rc == SQLITE_OK && db != NULL) {
        *ret = PTR_TO_HANDLE(db);
        return true;
    } else {
        *err = sqlite_to_wit_result(rc);
        if (db) sqlite3_close(db);
        return false;
    }
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_close(
    exports_sqlite_wasm_low_level_db_handle_t db
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    return sqlite_to_wit_result(sqlite3_close(pdb));
}

bool exports_sqlite_wasm_low_level_exec(
    exports_sqlite_wasm_low_level_db_handle_t db,
    sqlite_world_string_t *sql,
    sqlite_world_string_t *ret,
    exports_sqlite_wasm_low_level_result_code_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    char *sql_cstr = string_to_cstr(sql);
    char *errmsg = NULL;

    int rc = sqlite3_exec(pdb, sql_cstr ? sql_cstr : "", NULL, NULL, &errmsg);

    if (sql_cstr) free(sql_cstr);

    if (rc == SQLITE_OK) {
        /* Return empty string on success */
        ret->ptr = NULL;
        ret->len = 0;
        if (errmsg) sqlite3_free(errmsg);
        return true;
    } else {
        *err = sqlite_to_wit_result(rc);
        if (errmsg) sqlite3_free(errmsg);
        return false;
    }
}

bool exports_sqlite_wasm_low_level_prepare(
    exports_sqlite_wasm_low_level_db_handle_t db,
    sqlite_world_string_t *sql,
    exports_sqlite_wasm_low_level_stmt_handle_t *ret,
    exports_sqlite_wasm_low_level_result_code_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    sqlite3_stmt *stmt = NULL;
    char *sql_cstr = string_to_cstr(sql);

    int rc = sqlite3_prepare_v2(pdb, sql_cstr ? sql_cstr : "", -1, &stmt, NULL);

    if (sql_cstr) free(sql_cstr);

    if (rc == SQLITE_OK && stmt != NULL) {
        *ret = PTR_TO_HANDLE(stmt);
        return true;
    } else {
        *err = sqlite_to_wit_result(rc);
        return false;
    }
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_step(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(sqlite3_step(pstmt));
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_reset(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(sqlite3_reset(pstmt));
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_finalize(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(sqlite3_finalize(pstmt));
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_bind_null(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(sqlite3_bind_null(pstmt, index));
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_bind_int(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index,
    int32_t value
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(sqlite3_bind_int(pstmt, index, value));
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_bind_int64(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index,
    int64_t value
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(sqlite3_bind_int64(pstmt, index, value));
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_bind_double(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index,
    double value
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(sqlite3_bind_double(pstmt, index, value));
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_bind_text(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index,
    sqlite_world_string_t *value
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(
        sqlite3_bind_text(pstmt, index, (const char *)value->ptr, (int)value->len, SQLITE_TRANSIENT)
    );
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_bind_blob(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index,
    sqlite_world_list_u8_t *value
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(
        sqlite3_bind_blob(pstmt, index, value->ptr, (int)value->len, SQLITE_TRANSIENT)
    );
}

int32_t exports_sqlite_wasm_low_level_bind_parameter_count(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite3_bind_parameter_count(pstmt);
}

int32_t exports_sqlite_wasm_low_level_bind_parameter_index(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    sqlite_world_string_t *name
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    char *name_cstr = string_to_cstr(name);
    int idx = sqlite3_bind_parameter_index(pstmt, name_cstr ? name_cstr : "");
    if (name_cstr) free(name_cstr);
    return idx;
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_clear_bindings(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_result(sqlite3_clear_bindings(pstmt));
}

int32_t exports_sqlite_wasm_low_level_column_count(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite3_column_count(pstmt);
}

void exports_sqlite_wasm_low_level_column_name(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index,
    sqlite_world_string_t *ret
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    const char *name = sqlite3_column_name(pstmt, index);
    if (name) {
        sqlite_world_string_dup(ret, name);
    } else {
        ret->ptr = NULL;
        ret->len = 0;
    }
}

exports_sqlite_wasm_low_level_column_type_t exports_sqlite_wasm_low_level_get_column_type(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite_to_wit_column_type(sqlite3_column_type(pstmt, index));
}

int32_t exports_sqlite_wasm_low_level_column_int(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite3_column_int(pstmt, index);
}

int64_t exports_sqlite_wasm_low_level_column_int64(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite3_column_int64(pstmt, index);
}

double exports_sqlite_wasm_low_level_column_double(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite3_column_double(pstmt, index);
}

void exports_sqlite_wasm_low_level_column_text(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index,
    sqlite_world_string_t *ret
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    const unsigned char *text = sqlite3_column_text(pstmt, index);
    int len = sqlite3_column_bytes(pstmt, index);

    if (text && len > 0) {
        ret->ptr = (uint8_t *)malloc(len);
        if (ret->ptr) {
            memcpy(ret->ptr, text, len);
            ret->len = len;
        } else {
            ret->len = 0;
        }
    } else {
        ret->ptr = NULL;
        ret->len = 0;
    }
}

void exports_sqlite_wasm_low_level_column_blob(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index,
    sqlite_world_list_u8_t *ret
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    const void *blob = sqlite3_column_blob(pstmt, index);
    int len = sqlite3_column_bytes(pstmt, index);

    if (blob && len > 0) {
        ret->ptr = (uint8_t *)malloc(len);
        if (ret->ptr) {
            memcpy(ret->ptr, blob, len);
            ret->len = len;
        } else {
            ret->len = 0;
        }
    } else {
        ret->ptr = NULL;
        ret->len = 0;
    }
}

int32_t exports_sqlite_wasm_low_level_column_bytes(
    exports_sqlite_wasm_low_level_stmt_handle_t stmt,
    int32_t index
) {
    sqlite3_stmt *pstmt = HANDLE_TO_STMT(stmt);
    return sqlite3_column_bytes(pstmt, index);
}

void exports_sqlite_wasm_low_level_errmsg(
    exports_sqlite_wasm_low_level_db_handle_t db,
    sqlite_world_string_t *ret
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    const char *msg = sqlite3_errmsg(pdb);
    if (msg) {
        sqlite_world_string_dup(ret, msg);
    } else {
        ret->ptr = NULL;
        ret->len = 0;
    }
}

exports_sqlite_wasm_low_level_result_code_t exports_sqlite_wasm_low_level_errcode(
    exports_sqlite_wasm_low_level_db_handle_t db
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    return sqlite_to_wit_result(sqlite3_errcode(pdb));
}

int32_t exports_sqlite_wasm_low_level_extended_errcode(
    exports_sqlite_wasm_low_level_db_handle_t db
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    return sqlite3_extended_errcode(pdb);
}

bool exports_sqlite_wasm_low_level_get_autocommit(
    exports_sqlite_wasm_low_level_db_handle_t db
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    return sqlite3_get_autocommit(pdb) != 0;
}

int32_t exports_sqlite_wasm_low_level_changes(
    exports_sqlite_wasm_low_level_db_handle_t db
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    return sqlite3_changes(pdb);
}

int32_t exports_sqlite_wasm_low_level_total_changes(
    exports_sqlite_wasm_low_level_db_handle_t db
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    return sqlite3_total_changes(pdb);
}

int64_t exports_sqlite_wasm_low_level_last_insert_rowid(
    exports_sqlite_wasm_low_level_db_handle_t db
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    return sqlite3_last_insert_rowid(pdb);
}

void exports_sqlite_wasm_low_level_libversion(sqlite_world_string_t *ret) {
    sqlite_world_string_dup(ret, sqlite3_libversion());
}

int32_t exports_sqlite_wasm_low_level_libversion_number(void) {
    return sqlite3_libversion_number();
}

void exports_sqlite_wasm_low_level_sourceid(sqlite_world_string_t *ret) {
    sqlite_world_string_dup(ret, sqlite3_sourceid());
}
