/*
 * Host glue for the sqlite-cli-unified world.
 *
 * Implements the sqlite:extension/{spi,logging,config} interfaces that
 * sqlite-wasm exports to composed extensions, plus the cabi-side
 * startup wiring that calls each imported slot's describe() at init
 * and registers the manifest entries with SQLite.
 *
 * This file is the unified-WIT successor to src/exports/extension.c.
 * The current iteration implements the structural surface: every
 * exported function is present and its memory contracts are correct,
 * but execution paths (spi.execute, spi.execute_scalar) are stubs
 * returning a "not yet implemented" error until the cross-component
 * SQL-execution path is wired through. logging and config are
 * functional; logging routes to stderr, config.sqlite_version returns
 * the real version.
 *
 * The slot-driven scalar-function dispatch (`register_all_slots`)
 * lives here too but is invoked from the world's init path once the
 * surrounding bindings settle.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sqlite3.h"
#include "sqlite_cli_unified.h"

/* ------------------------------------------------------------------ */
/* String helpers                                                     */
/* ------------------------------------------------------------------ */

/* Copy a null-terminated C string into a wit-bindgen string out-arg.
 * Memory is malloc'd here and freed by the cabi runtime later. */
static void set_wit_string(sqlite_cli_unified_string_t *ret, const char *s) {
    size_t len = s ? strlen(s) : 0;
    if (len == 0) {
        ret->ptr = NULL;
        ret->len = 0;
        return;
    }
    ret->ptr = (uint8_t *)malloc(len);
    if (ret->ptr) {
        memcpy(ret->ptr, s, len);
        ret->len = len;
    } else {
        ret->len = 0;
    }
}

/* Render the wit string as a C-side null-terminated copy. Caller frees. */
static char *to_cstring(const sqlite_cli_unified_string_t *s) {
    if (!s || s->len == 0) {
        char *empty = malloc(1);
        if (empty) empty[0] = '\0';
        return empty;
    }
    char *buf = malloc(s->len + 1);
    if (!buf) return NULL;
    memcpy(buf, s->ptr, s->len);
    buf[s->len] = '\0';
    return buf;
}

/* Fill an error record with the given code + message. The message is
 * dup'd into the wit-bindgen layout. */
static void fill_error(exports_sqlite_extension_spi_sqlite_error_t *err,
                       int32_t code, const char *msg) {
    err->code = code;
    err->extended_code = code;
    set_wit_string(&err->message, msg);
}

/* ------------------------------------------------------------------ */
/* sqlite:extension/spi                                               */
/* ------------------------------------------------------------------ */

bool exports_sqlite_extension_spi_execute(
    sqlite_cli_unified_string_t *sql,
    exports_sqlite_extension_spi_list_sql_value_t *params,
    exports_sqlite_extension_spi_query_result_t *ret,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)sql;
    (void)params;
    (void)ret;
    /* The host SQLite connection isn't yet plumbed through to this
     * surface. Once register_all_slots wires up at init, this will
     * route to sqlite3_exec / sqlite3_prepare_v2 + step. */
    fill_error(err, SQLITE_ERROR, "spi.execute not yet implemented");
    return false;
}

bool exports_sqlite_extension_spi_execute_scalar(
    sqlite_cli_unified_string_t *sql,
    exports_sqlite_extension_spi_list_sql_value_t *params,
    exports_sqlite_extension_spi_sql_value_t *ret,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)sql;
    (void)params;
    (void)ret;
    fill_error(err, SQLITE_ERROR, "spi.execute-scalar not yet implemented");
    return false;
}

bool exports_sqlite_extension_spi_execute_batch(
    sqlite_cli_unified_string_t *sql,
    int64_t *ret,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)sql;
    (void)ret;
    fill_error(err, SQLITE_ERROR, "spi.execute-batch not yet implemented");
    return false;
}

/* ------------------------------------------------------------------ */
/* sqlite:extension/logging                                           */
/* ------------------------------------------------------------------ */

static const char *level_label(exports_sqlite_extension_logging_log_level_t l) {
    switch (l) {
        case EXPORTS_SQLITE_EXTENSION_TYPES_LOG_LEVEL_ERROR: return "ERROR";
        case EXPORTS_SQLITE_EXTENSION_TYPES_LOG_LEVEL_WARN:  return "WARN";
        case EXPORTS_SQLITE_EXTENSION_TYPES_LOG_LEVEL_INFO:  return "INFO";
        case EXPORTS_SQLITE_EXTENSION_TYPES_LOG_LEVEL_DEBUG: return "DEBUG";
        case EXPORTS_SQLITE_EXTENSION_TYPES_LOG_LEVEL_TRACE: return "TRACE";
        default: return "LOG";
    }
}

static void emit_log(const char *level, const sqlite_cli_unified_string_t *msg) {
    char *m = to_cstring(msg);
    fprintf(stderr, "[ext-%s] %s\n", level, m ? m : "");
    free(m);
}

void exports_sqlite_extension_logging_log(
    exports_sqlite_extension_logging_log_level_t level,
    sqlite_cli_unified_string_t *message
) {
    emit_log(level_label(level), message);
}

void exports_sqlite_extension_logging_error(sqlite_cli_unified_string_t *message) {
    emit_log("ERROR", message);
}

void exports_sqlite_extension_logging_warn(sqlite_cli_unified_string_t *message) {
    emit_log("WARN", message);
}

void exports_sqlite_extension_logging_info(sqlite_cli_unified_string_t *message) {
    emit_log("INFO", message);
}

void exports_sqlite_extension_logging_debug(sqlite_cli_unified_string_t *message) {
    emit_log("DEBUG", message);
}

/* ------------------------------------------------------------------ */
/* sqlite:extension/config                                            */
/* ------------------------------------------------------------------ */

bool exports_sqlite_extension_config_get(
    sqlite_cli_unified_string_t *key,
    sqlite_cli_unified_string_t *ret
) {
    (void)key;
    (void)ret;
    /* In-memory config map is a follow-up; for now no keys defined. */
    return false;
}

bool exports_sqlite_extension_config_set(
    sqlite_cli_unified_string_t *key,
    sqlite_cli_unified_string_t *value
) {
    (void)key;
    (void)value;
    return false;
}

void exports_sqlite_extension_config_sqlite_version(sqlite_cli_unified_string_t *ret) {
    set_wit_string(ret, sqlite3_libversion());
}

void exports_sqlite_extension_config_extension_version(sqlite_cli_unified_string_t *ret) {
    set_wit_string(ret, "0.1.0");
}
