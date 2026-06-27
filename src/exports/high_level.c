/*
 * SQLite WASM High-Level API Implementation
 *
 * This file provides the resource-based high-level API implementation
 * matching the wit-bindgen generated function signatures.
 */

#include <stdlib.h>
#include <string.h>
#include "sqlite3.h"
#include "sqlite_world.h"

/*
 * Connection resource - holds SQLite database connection
 */
struct exports_sqlink_wasm_high_level_connection_t {
    sqlite3 *db;
    int last_error_code;
    int last_extended_error_code;
    char *last_error_message;
};

/*
 * Statement resource - holds prepared statement
 */
struct exports_sqlink_wasm_high_level_statement_t {
    sqlite3_stmt *stmt;
    exports_sqlink_wasm_high_level_connection_t *conn;
    int column_count;
};

/*
 * Helper functions
 */

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

static void set_connection_error(exports_sqlink_wasm_high_level_connection_t *conn) {
    if (!conn || !conn->db) return;

    if (conn->last_error_message) {
        free(conn->last_error_message);
    }

    conn->last_error_code = sqlite3_errcode(conn->db);
    conn->last_extended_error_code = sqlite3_extended_errcode(conn->db);

    const char *msg = sqlite3_errmsg(conn->db);
    if (msg) {
        conn->last_error_message = strdup(msg);
    } else {
        conn->last_error_message = NULL;
    }
}

static void fill_error(
    exports_sqlink_wasm_high_level_database_error_t *err,
    int code,
    int extended_code,
    const char *message
) {
    err->code = code;
    err->extended_code = extended_code;
    if (message) {
        sqlite_world_string_dup(&err->message, message);
    } else {
        err->message.ptr = NULL;
        err->message.len = 0;
    }
}

static void fill_error_from_conn(
    exports_sqlink_wasm_high_level_database_error_t *err,
    exports_sqlink_wasm_high_level_connection_t *conn
) {
    fill_error(err, conn->last_error_code, conn->last_extended_error_code,
               conn->last_error_message);
}

static int bind_value(sqlite3_stmt *stmt, int index, exports_sqlink_wasm_high_level_value_t *val) {
    switch (val->tag) {
        case EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_NULL:
            return sqlite3_bind_null(stmt, index);
        case EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_INTEGER:
            return sqlite3_bind_int64(stmt, index, val->val.integer);
        case EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_REAL:
            return sqlite3_bind_double(stmt, index, val->val.real);
        case EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_TEXT:
            return sqlite3_bind_text(stmt, index, (const char *)val->val.text.ptr,
                                     (int)val->val.text.len, SQLITE_TRANSIENT);
        case EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_BLOB:
            return sqlite3_bind_blob(stmt, index, val->val.blob.ptr,
                                     (int)val->val.blob.len, SQLITE_TRANSIENT);
        default:
            return SQLITE_MISUSE;
    }
}

static void read_column(
    sqlite3_stmt *stmt,
    int index,
    exports_sqlink_wasm_high_level_value_t *val
) {
    int type = sqlite3_column_type(stmt, index);

    switch (type) {
        case SQLITE_INTEGER:
            val->tag = EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_INTEGER;
            val->val.integer = sqlite3_column_int64(stmt, index);
            break;

        case SQLITE_FLOAT:
            val->tag = EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_REAL;
            val->val.real = sqlite3_column_double(stmt, index);
            break;

        case SQLITE_TEXT: {
            const unsigned char *text = sqlite3_column_text(stmt, index);
            int len = sqlite3_column_bytes(stmt, index);
            val->tag = EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_TEXT;
            if (text && len > 0) {
                val->val.text.ptr = (uint8_t *)malloc(len);
                if (val->val.text.ptr) {
                    memcpy(val->val.text.ptr, text, len);
                    val->val.text.len = len;
                } else {
                    val->val.text.len = 0;
                }
            } else {
                val->val.text.ptr = NULL;
                val->val.text.len = 0;
            }
            break;
        }

        case SQLITE_BLOB: {
            const void *blob = sqlite3_column_blob(stmt, index);
            int len = sqlite3_column_bytes(stmt, index);
            val->tag = EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_BLOB;
            if (blob && len > 0) {
                val->val.blob.ptr = (uint8_t *)malloc(len);
                if (val->val.blob.ptr) {
                    memcpy(val->val.blob.ptr, blob, len);
                    val->val.blob.len = len;
                } else {
                    val->val.blob.len = 0;
                }
            } else {
                val->val.blob.ptr = NULL;
                val->val.blob.len = 0;
            }
            break;
        }

        case SQLITE_NULL:
        default:
            val->tag = EXPORTS_SQLINK_WASM_HIGH_LEVEL_VALUE_NULL;
            break;
    }
}

/*
 * Connection constructor
 */
exports_sqlink_wasm_high_level_own_connection_t
exports_sqlink_wasm_high_level_constructor_connection(
    sqlite_world_string_t *path,
    exports_sqlink_wasm_high_level_open_mode_t mode
) {
    exports_sqlink_wasm_high_level_connection_t *conn =
        (exports_sqlink_wasm_high_level_connection_t *)malloc(sizeof(*conn));

    if (!conn) {
        return exports_sqlink_wasm_high_level_connection_new(NULL);
    }

    memset(conn, 0, sizeof(*conn));

    int flags = 0;
    switch (mode) {
        case EXPORTS_SQLINK_WASM_HIGH_LEVEL_OPEN_MODE_READ_ONLY:
            flags = SQLITE_OPEN_READONLY;
            break;
        case EXPORTS_SQLINK_WASM_HIGH_LEVEL_OPEN_MODE_READ_WRITE:
            flags = SQLITE_OPEN_READWRITE;
            break;
        case EXPORTS_SQLINK_WASM_HIGH_LEVEL_OPEN_MODE_READ_WRITE_CREATE:
            flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE;
            break;
        case EXPORTS_SQLINK_WASM_HIGH_LEVEL_OPEN_MODE_MEMORY:
            flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_MEMORY;
            break;
        default:
            flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE;
            break;
    }

    char *path_cstr = string_to_cstr(path);
    /* File-backed modes must use the WASI VFS so the database
     * actually hits the filesystem; the default VFS is the in-memory
     * memvfs. Memory mode (and a missing path) stay on the default. */
    const char *vfs =
        (mode == EXPORTS_SQLINK_WASM_HIGH_LEVEL_OPEN_MODE_MEMORY || !path_cstr)
            ? NULL
            : "wasivfs";
    int rc = sqlite3_open_v2(path_cstr ? path_cstr : ":memory:", &conn->db, flags, vfs);
    if (path_cstr) free(path_cstr);

    if (rc != SQLITE_OK) {
        set_connection_error(conn);
    }

    return exports_sqlink_wasm_high_level_connection_new(conn);
}

/*
 * Connection destructor
 */
void exports_sqlink_wasm_high_level_connection_destructor(
    exports_sqlink_wasm_high_level_connection_t *rep
) {
    if (rep) {
        if (rep->db) {
            sqlite3_close(rep->db);
        }
        if (rep->last_error_message) {
            free(rep->last_error_message);
        }
        free(rep);
    }
}

/*
 * Connection methods
 */

bool exports_sqlink_wasm_high_level_method_connection_execute(
    exports_sqlink_wasm_high_level_borrow_connection_t self,
    sqlite_world_string_t *sql,
    exports_sqlink_wasm_high_level_exec_result_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->db) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Connection is closed");
        return false;
    }

    char *sql_cstr = string_to_cstr(sql);
    char *errmsg = NULL;

    int rc = sqlite3_exec(self->db, sql_cstr ? sql_cstr : "", NULL, NULL, &errmsg);

    if (sql_cstr) free(sql_cstr);

    if (rc == SQLITE_OK) {
        ret->changes = sqlite3_changes(self->db);
        ret->last_insert_rowid = sqlite3_last_insert_rowid(self->db);
        if (errmsg) sqlite3_free(errmsg);
        return true;
    } else {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        if (errmsg) sqlite3_free(errmsg);
        return false;
    }
}

bool exports_sqlink_wasm_high_level_method_connection_execute_with_params(
    exports_sqlink_wasm_high_level_borrow_connection_t self,
    sqlite_world_string_t *sql,
    exports_sqlink_wasm_high_level_list_value_t *params,
    exports_sqlink_wasm_high_level_exec_result_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->db) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Connection is closed");
        return false;
    }

    char *sql_cstr = string_to_cstr(sql);
    sqlite3_stmt *stmt = NULL;

    int rc = sqlite3_prepare_v2(self->db, sql_cstr ? sql_cstr : "", -1, &stmt, NULL);
    if (sql_cstr) free(sql_cstr);

    if (rc != SQLITE_OK) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        return false;
    }

    /* Bind parameters */
    for (size_t i = 0; i < params->len; i++) {
        rc = bind_value(stmt, (int)(i + 1), &params->ptr[i]);
        if (rc != SQLITE_OK) {
            set_connection_error(self);
            fill_error_from_conn(err, self);
            sqlite3_finalize(stmt);
            return false;
        }
    }

    /* Execute */
    rc = sqlite3_step(stmt);
    if (rc != SQLITE_DONE && rc != SQLITE_ROW) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        sqlite3_finalize(stmt);
        return false;
    }

    ret->changes = sqlite3_changes(self->db);
    ret->last_insert_rowid = sqlite3_last_insert_rowid(self->db);

    sqlite3_finalize(stmt);
    return true;
}

bool exports_sqlink_wasm_high_level_method_connection_query(
    exports_sqlink_wasm_high_level_borrow_connection_t self,
    sqlite_world_string_t *sql,
    exports_sqlink_wasm_high_level_query_result_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->db) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Connection is closed");
        return false;
    }

    char *sql_cstr = string_to_cstr(sql);
    sqlite3_stmt *stmt = NULL;

    int rc = sqlite3_prepare_v2(self->db, sql_cstr ? sql_cstr : "", -1, &stmt, NULL);
    if (sql_cstr) free(sql_cstr);

    if (rc != SQLITE_OK) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        return false;
    }

    /* Get column info */
    int col_count = sqlite3_column_count(stmt);
    ret->column_names.ptr = (sqlite_world_string_t *)malloc(sizeof(sqlite_world_string_t) * col_count);
    ret->column_names.len = col_count;

    for (int i = 0; i < col_count; i++) {
        const char *name = sqlite3_column_name(stmt, i);
        if (name) {
            sqlite_world_string_dup(&ret->column_names.ptr[i], name);
        } else {
            ret->column_names.ptr[i].ptr = NULL;
            ret->column_names.ptr[i].len = 0;
        }
    }

    /* Collect rows */
    size_t row_capacity = 16;
    ret->rows.ptr = (exports_sqlink_wasm_high_level_row_t *)malloc(
        sizeof(exports_sqlink_wasm_high_level_row_t) * row_capacity);
    ret->rows.len = 0;

    while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
        /* Grow rows array if needed */
        if (ret->rows.len >= row_capacity) {
            row_capacity *= 2;
            ret->rows.ptr = (exports_sqlink_wasm_high_level_row_t *)realloc(
                ret->rows.ptr,
                sizeof(exports_sqlink_wasm_high_level_row_t) * row_capacity);
        }

        /* Read row */
        exports_sqlink_wasm_high_level_row_t *row = &ret->rows.ptr[ret->rows.len];
        row->columns.ptr = (exports_sqlink_wasm_high_level_value_t *)malloc(
            sizeof(exports_sqlink_wasm_high_level_value_t) * col_count);
        row->columns.len = col_count;

        for (int i = 0; i < col_count; i++) {
            read_column(stmt, i, &row->columns.ptr[i]);
        }

        ret->rows.len++;
    }

    if (rc != SQLITE_DONE) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        sqlite3_finalize(stmt);
        exports_sqlink_wasm_high_level_query_result_free(ret);
        return false;
    }

    sqlite3_finalize(stmt);
    return true;
}

bool exports_sqlink_wasm_high_level_method_connection_query_with_params(
    exports_sqlink_wasm_high_level_borrow_connection_t self,
    sqlite_world_string_t *sql,
    exports_sqlink_wasm_high_level_list_value_t *params,
    exports_sqlink_wasm_high_level_query_result_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->db) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Connection is closed");
        return false;
    }

    char *sql_cstr = string_to_cstr(sql);
    sqlite3_stmt *stmt = NULL;

    int rc = sqlite3_prepare_v2(self->db, sql_cstr ? sql_cstr : "", -1, &stmt, NULL);
    if (sql_cstr) free(sql_cstr);

    if (rc != SQLITE_OK) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        return false;
    }

    /* Bind parameters */
    for (size_t i = 0; i < params->len; i++) {
        rc = bind_value(stmt, (int)(i + 1), &params->ptr[i]);
        if (rc != SQLITE_OK) {
            set_connection_error(self);
            fill_error_from_conn(err, self);
            sqlite3_finalize(stmt);
            return false;
        }
    }

    /* Get column info */
    int col_count = sqlite3_column_count(stmt);
    ret->column_names.ptr = (sqlite_world_string_t *)malloc(sizeof(sqlite_world_string_t) * col_count);
    ret->column_names.len = col_count;

    for (int i = 0; i < col_count; i++) {
        const char *name = sqlite3_column_name(stmt, i);
        if (name) {
            sqlite_world_string_dup(&ret->column_names.ptr[i], name);
        } else {
            ret->column_names.ptr[i].ptr = NULL;
            ret->column_names.ptr[i].len = 0;
        }
    }

    /* Collect rows */
    size_t row_capacity = 16;
    ret->rows.ptr = (exports_sqlink_wasm_high_level_row_t *)malloc(
        sizeof(exports_sqlink_wasm_high_level_row_t) * row_capacity);
    ret->rows.len = 0;

    while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
        if (ret->rows.len >= row_capacity) {
            row_capacity *= 2;
            ret->rows.ptr = (exports_sqlink_wasm_high_level_row_t *)realloc(
                ret->rows.ptr,
                sizeof(exports_sqlink_wasm_high_level_row_t) * row_capacity);
        }

        exports_sqlink_wasm_high_level_row_t *row = &ret->rows.ptr[ret->rows.len];
        row->columns.ptr = (exports_sqlink_wasm_high_level_value_t *)malloc(
            sizeof(exports_sqlink_wasm_high_level_value_t) * col_count);
        row->columns.len = col_count;

        for (int i = 0; i < col_count; i++) {
            read_column(stmt, i, &row->columns.ptr[i]);
        }

        ret->rows.len++;
    }

    if (rc != SQLITE_DONE) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        sqlite3_finalize(stmt);
        exports_sqlink_wasm_high_level_query_result_free(ret);
        return false;
    }

    sqlite3_finalize(stmt);
    return true;
}

bool exports_sqlink_wasm_high_level_method_connection_prepare(
    exports_sqlink_wasm_high_level_borrow_connection_t self,
    sqlite_world_string_t *sql,
    exports_sqlink_wasm_high_level_own_statement_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->db) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Connection is closed");
        return false;
    }

    exports_sqlink_wasm_high_level_statement_t *stmt_obj =
        (exports_sqlink_wasm_high_level_statement_t *)malloc(sizeof(*stmt_obj));

    if (!stmt_obj) {
        fill_error(err, SQLITE_NOMEM, SQLITE_NOMEM, "Out of memory");
        return false;
    }

    char *sql_cstr = string_to_cstr(sql);
    int rc = sqlite3_prepare_v2(self->db, sql_cstr ? sql_cstr : "", -1, &stmt_obj->stmt, NULL);
    if (sql_cstr) free(sql_cstr);

    if (rc != SQLITE_OK) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        free(stmt_obj);
        return false;
    }

    stmt_obj->conn = self;
    stmt_obj->column_count = sqlite3_column_count(stmt_obj->stmt);

    *ret = exports_sqlink_wasm_high_level_statement_new(stmt_obj);
    return true;
}

bool exports_sqlink_wasm_high_level_method_connection_begin_transaction(
    exports_sqlink_wasm_high_level_borrow_connection_t self,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->db) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Connection is closed");
        return false;
    }

    char *errmsg = NULL;
    int rc = sqlite3_exec(self->db, "BEGIN", NULL, NULL, &errmsg);

    if (rc != SQLITE_OK) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        if (errmsg) sqlite3_free(errmsg);
        return false;
    }

    if (errmsg) sqlite3_free(errmsg);
    return true;
}

bool exports_sqlink_wasm_high_level_method_connection_commit(
    exports_sqlink_wasm_high_level_borrow_connection_t self,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->db) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Connection is closed");
        return false;
    }

    char *errmsg = NULL;
    int rc = sqlite3_exec(self->db, "COMMIT", NULL, NULL, &errmsg);

    if (rc != SQLITE_OK) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        if (errmsg) sqlite3_free(errmsg);
        return false;
    }

    if (errmsg) sqlite3_free(errmsg);
    return true;
}

bool exports_sqlink_wasm_high_level_method_connection_rollback(
    exports_sqlink_wasm_high_level_borrow_connection_t self,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->db) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Connection is closed");
        return false;
    }

    char *errmsg = NULL;
    int rc = sqlite3_exec(self->db, "ROLLBACK", NULL, NULL, &errmsg);

    if (rc != SQLITE_OK) {
        set_connection_error(self);
        fill_error_from_conn(err, self);
        if (errmsg) sqlite3_free(errmsg);
        return false;
    }

    if (errmsg) sqlite3_free(errmsg);
    return true;
}

bool exports_sqlink_wasm_high_level_method_connection_in_autocommit(
    exports_sqlink_wasm_high_level_borrow_connection_t self
) {
    if (!self || !self->db) return true;
    return sqlite3_get_autocommit(self->db) != 0;
}

bool exports_sqlink_wasm_high_level_method_connection_last_error(
    exports_sqlink_wasm_high_level_borrow_connection_t self,
    exports_sqlink_wasm_high_level_database_error_t *ret
) {
    if (!self || !self->last_error_message) {
        return false; /* Option::None */
    }

    fill_error_from_conn(ret, self);
    return true; /* Option::Some */
}

/*
 * Statement destructor
 */
void exports_sqlink_wasm_high_level_statement_destructor(
    exports_sqlink_wasm_high_level_statement_t *rep
) {
    if (rep) {
        if (rep->stmt) {
            sqlite3_finalize(rep->stmt);
        }
        free(rep);
    }
}

/*
 * Statement methods
 */

bool exports_sqlink_wasm_high_level_method_statement_bind(
    exports_sqlink_wasm_high_level_borrow_statement_t self,
    int32_t index,
    exports_sqlink_wasm_high_level_value_t *value,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->stmt) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Statement is closed");
        return false;
    }

    int rc = bind_value(self->stmt, index, value);
    if (rc != SQLITE_OK) {
        set_connection_error(self->conn);
        fill_error_from_conn(err, self->conn);
        return false;
    }

    return true;
}

bool exports_sqlink_wasm_high_level_method_statement_bind_all(
    exports_sqlink_wasm_high_level_borrow_statement_t self,
    exports_sqlink_wasm_high_level_list_value_t *params,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->stmt) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Statement is closed");
        return false;
    }

    for (size_t i = 0; i < params->len; i++) {
        int rc = bind_value(self->stmt, (int)(i + 1), &params->ptr[i]);
        if (rc != SQLITE_OK) {
            set_connection_error(self->conn);
            fill_error_from_conn(err, self->conn);
            return false;
        }
    }

    return true;
}

bool exports_sqlink_wasm_high_level_method_statement_execute(
    exports_sqlink_wasm_high_level_borrow_statement_t self,
    exports_sqlink_wasm_high_level_exec_result_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->stmt) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Statement is closed");
        return false;
    }

    int rc = sqlite3_step(self->stmt);
    if (rc != SQLITE_DONE && rc != SQLITE_ROW) {
        set_connection_error(self->conn);
        fill_error_from_conn(err, self->conn);
        return false;
    }

    ret->changes = sqlite3_changes(self->conn->db);
    ret->last_insert_rowid = sqlite3_last_insert_rowid(self->conn->db);

    return true;
}

bool exports_sqlink_wasm_high_level_method_statement_query(
    exports_sqlink_wasm_high_level_borrow_statement_t self,
    exports_sqlink_wasm_high_level_query_result_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->stmt) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Statement is closed");
        return false;
    }

    int col_count = self->column_count;

    /* Get column names */
    ret->column_names.ptr = (sqlite_world_string_t *)malloc(sizeof(sqlite_world_string_t) * col_count);
    ret->column_names.len = col_count;

    for (int i = 0; i < col_count; i++) {
        const char *name = sqlite3_column_name(self->stmt, i);
        if (name) {
            sqlite_world_string_dup(&ret->column_names.ptr[i], name);
        } else {
            ret->column_names.ptr[i].ptr = NULL;
            ret->column_names.ptr[i].len = 0;
        }
    }

    /* Collect rows */
    size_t row_capacity = 16;
    ret->rows.ptr = (exports_sqlink_wasm_high_level_row_t *)malloc(
        sizeof(exports_sqlink_wasm_high_level_row_t) * row_capacity);
    ret->rows.len = 0;

    int rc;
    while ((rc = sqlite3_step(self->stmt)) == SQLITE_ROW) {
        if (ret->rows.len >= row_capacity) {
            row_capacity *= 2;
            ret->rows.ptr = (exports_sqlink_wasm_high_level_row_t *)realloc(
                ret->rows.ptr,
                sizeof(exports_sqlink_wasm_high_level_row_t) * row_capacity);
        }

        exports_sqlink_wasm_high_level_row_t *row = &ret->rows.ptr[ret->rows.len];
        row->columns.ptr = (exports_sqlink_wasm_high_level_value_t *)malloc(
            sizeof(exports_sqlink_wasm_high_level_value_t) * col_count);
        row->columns.len = col_count;

        for (int i = 0; i < col_count; i++) {
            read_column(self->stmt, i, &row->columns.ptr[i]);
        }

        ret->rows.len++;
    }

    if (rc != SQLITE_DONE) {
        set_connection_error(self->conn);
        fill_error_from_conn(err, self->conn);
        exports_sqlink_wasm_high_level_query_result_free(ret);
        return false;
    }

    return true;
}

bool exports_sqlink_wasm_high_level_method_statement_step(
    exports_sqlink_wasm_high_level_borrow_statement_t self,
    exports_sqlink_wasm_high_level_option_row_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->stmt) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Statement is closed");
        return false;
    }

    int rc = sqlite3_step(self->stmt);

    if (rc == SQLITE_ROW) {
        ret->is_some = true;
        ret->val.columns.ptr = (exports_sqlink_wasm_high_level_value_t *)malloc(
            sizeof(exports_sqlink_wasm_high_level_value_t) * self->column_count);
        ret->val.columns.len = self->column_count;

        for (int i = 0; i < self->column_count; i++) {
            read_column(self->stmt, i, &ret->val.columns.ptr[i]);
        }
        return true;
    } else if (rc == SQLITE_DONE) {
        ret->is_some = false;
        return true;
    } else {
        set_connection_error(self->conn);
        fill_error_from_conn(err, self->conn);
        return false;
    }
}

bool exports_sqlink_wasm_high_level_method_statement_reset(
    exports_sqlink_wasm_high_level_borrow_statement_t self,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->stmt) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Statement is closed");
        return false;
    }

    int rc = sqlite3_reset(self->stmt);
    if (rc != SQLITE_OK) {
        set_connection_error(self->conn);
        fill_error_from_conn(err, self->conn);
        return false;
    }

    return true;
}

bool exports_sqlink_wasm_high_level_method_statement_clear_bindings(
    exports_sqlink_wasm_high_level_borrow_statement_t self,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (!self || !self->stmt) {
        fill_error(err, SQLITE_MISUSE, SQLITE_MISUSE, "Statement is closed");
        return false;
    }

    int rc = sqlite3_clear_bindings(self->stmt);
    if (rc != SQLITE_OK) {
        set_connection_error(self->conn);
        fill_error_from_conn(err, self->conn);
        return false;
    }

    return true;
}

int32_t exports_sqlink_wasm_high_level_method_statement_column_count(
    exports_sqlink_wasm_high_level_borrow_statement_t self
) {
    if (!self || !self->stmt) return 0;
    return self->column_count;
}

void exports_sqlink_wasm_high_level_method_statement_column_names(
    exports_sqlink_wasm_high_level_borrow_statement_t self,
    sqlite_world_list_string_t *ret
) {
    if (!self || !self->stmt) {
        ret->ptr = NULL;
        ret->len = 0;
        return;
    }

    ret->ptr = (sqlite_world_string_t *)malloc(sizeof(sqlite_world_string_t) * self->column_count);
    ret->len = self->column_count;

    for (int i = 0; i < self->column_count; i++) {
        const char *name = sqlite3_column_name(self->stmt, i);
        if (name) {
            sqlite_world_string_dup(&ret->ptr[i], name);
        } else {
            ret->ptr[i].ptr = NULL;
            ret->ptr[i].len = 0;
        }
    }
}

int32_t exports_sqlink_wasm_high_level_method_statement_parameter_count(
    exports_sqlink_wasm_high_level_borrow_statement_t self
) {
    if (!self || !self->stmt) return 0;
    return sqlite3_bind_parameter_count(self->stmt);
}

/*
 * Utility functions
 */

void exports_sqlink_wasm_high_level_version(sqlite_world_string_t *ret) {
    sqlite_world_string_dup(ret, sqlite3_libversion());
}

int32_t exports_sqlink_wasm_high_level_version_number(void) {
    return sqlite3_libversion_number();
}

// Singleton tracker for the high-level `default-connection` getter.
// Per WIT contract (wit/sqlite-high-level.wit): "The default connection
// is created on first call and lives for the lifetime of the component
// instance." This is the SPI-side shared connection that backs both
// SPI and the high-level handle.
static exports_sqlink_wasm_high_level_connection_t *g_default_connection = NULL;

bool exports_sqlink_wasm_high_level_default_connection(
    exports_sqlink_wasm_high_level_own_connection_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    if (g_default_connection == NULL) {
        g_default_connection = (exports_sqlink_wasm_high_level_connection_t *)
            malloc(sizeof(*g_default_connection));
        if (!g_default_connection) {
            fill_error(err, SQLITE_NOMEM, SQLITE_NOMEM, "Out of memory");
            return false;
        }
        memset(g_default_connection, 0, sizeof(*g_default_connection));
        int rc = sqlite3_open_v2(":memory:", &g_default_connection->db,
                                  SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE
                                      | SQLITE_OPEN_MEMORY,
                                  NULL);
        if (rc != SQLITE_OK) {
            set_connection_error(g_default_connection);
            fill_error_from_conn(err, g_default_connection);
            // Don't run the destructor — we want the singleton to retry
            // next call. Free the unopened struct directly.
            free(g_default_connection);
            g_default_connection = NULL;
            return false;
        }
    }
    *ret = exports_sqlink_wasm_high_level_connection_new(g_default_connection);
    return true;
}

bool exports_sqlink_wasm_high_level_open_memory(
    exports_sqlink_wasm_high_level_own_connection_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    exports_sqlink_wasm_high_level_connection_t *conn =
        (exports_sqlink_wasm_high_level_connection_t *)malloc(sizeof(*conn));

    if (!conn) {
        fill_error(err, SQLITE_NOMEM, SQLITE_NOMEM, "Out of memory");
        return false;
    }

    memset(conn, 0, sizeof(*conn));

    int rc = sqlite3_open_v2(":memory:", &conn->db,
                             SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_MEMORY,
                             NULL);

    if (rc != SQLITE_OK) {
        set_connection_error(conn);
        fill_error_from_conn(err, conn);
        exports_sqlink_wasm_high_level_connection_destructor(conn);
        return false;
    }

    *ret = exports_sqlink_wasm_high_level_connection_new(conn);
    return true;
}

bool exports_sqlink_wasm_high_level_open_file(
    sqlite_world_string_t *path,
    exports_sqlink_wasm_high_level_own_connection_t *ret,
    exports_sqlink_wasm_high_level_database_error_t *err
) {
    exports_sqlink_wasm_high_level_connection_t *conn =
        (exports_sqlink_wasm_high_level_connection_t *)malloc(sizeof(*conn));

    if (!conn) {
        fill_error(err, SQLITE_NOMEM, SQLITE_NOMEM, "Out of memory");
        return false;
    }

    memset(conn, 0, sizeof(*conn));

    char *path_cstr = string_to_cstr(path);
    /* open_file is explicitly file-backed: use the WASI VFS (the
     * default VFS is the in-memory memvfs, which would silently drop
     * the file). Fall back to the default only if no path is given. */
    const char *vfs = path_cstr ? "wasivfs" : NULL;
    int rc = sqlite3_open_v2(path_cstr ? path_cstr : ":memory:", &conn->db,
                             SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
                             vfs);
    if (path_cstr) free(path_cstr);

    if (rc != SQLITE_OK) {
        set_connection_error(conn);
        fill_error_from_conn(err, conn);
        exports_sqlink_wasm_high_level_connection_destructor(conn);
        return false;
    }

    *ret = exports_sqlink_wasm_high_level_connection_new(conn);
    return true;
}
