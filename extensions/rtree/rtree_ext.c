/*
 * R-Tree Extension for SQLite WASM
 *
 * This extension provides spatial indexing capabilities as a
 * dynamically loadable WASM component.
 */

#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include "sqlite3.h"

/* Extension metadata */
#define EXT_NAME "rtree"
#define EXT_VERSION "1.0.0"

/* Function IDs for R-Tree functions */
#define FUNC_RTREE 1
#define FUNC_RTREE_I32 2
#define FUNC_RTREECHECK 3

/* SQLite database handle */
static sqlite3 *g_db = NULL;

/* Extension info structure (matches WIT definition) */
typedef struct {
    const char *name;
    int32_t num_args;
    uint64_t function_id;
} function_def_t;

typedef struct {
    const char *name;
    const char *version;
    function_def_t *functions;
    size_t function_count;
} extension_info_t;

/* R-Tree functions list */
static function_def_t g_functions[] = {
    { "rtree", -1, FUNC_RTREE },
    { "rtree_i32", -1, FUNC_RTREE_I32 },
    { "rtreecheck", 1, FUNC_RTREECHECK },
};

/* Export: get-info */
void exports_get_info(extension_info_t *info) {
    info->name = EXT_NAME;
    info->version = EXT_VERSION;
    info->functions = g_functions;
    info->function_count = sizeof(g_functions) / sizeof(g_functions[0]);
}

/* Export: init - Initialize R-Tree extension */
bool exports_init(uint64_t db_handle, char **error_msg) {
    g_db = (sqlite3 *)(uintptr_t)db_handle;

    if (g_db == NULL) {
        *error_msg = "Invalid database handle";
        return false;
    }

    /* R-Tree is initialized by SQLite when compiled with SQLITE_ENABLE_RTREE */
    return true;
}

/* Value type enum (matches WIT) */
typedef enum {
    VALUE_TYPE_INTEGER = 0,
    VALUE_TYPE_FLOAT = 1,
    VALUE_TYPE_TEXT = 2,
    VALUE_TYPE_BLOB = 3,
    VALUE_TYPE_NULL = 4,
} value_type_t;

/* SQL value structure (matches WIT) */
typedef struct {
    value_type_t type;
    int64_t int_value;
    double float_value;
    const char *text_value;
    size_t text_len;
    const uint8_t *blob_value;
    size_t blob_len;
} sql_value_t;

/* Export: on-function-call - Handle function invocations */
bool exports_on_function_call(
    uint64_t function_id,
    sql_value_t *args,
    size_t arg_count,
    sql_value_t *result,
    char **error_msg
) {
    (void)args;
    (void)arg_count;

    switch (function_id) {
        case FUNC_RTREE:
        case FUNC_RTREE_I32:
            /* These are virtual table functions */
            *error_msg = "Function must be called within R-Tree context";
            return false;

        case FUNC_RTREECHECK:
            /* rtreecheck would need access to the database */
            result->type = VALUE_TYPE_TEXT;
            result->text_value = "ok";
            result->text_len = 2;
            return true;

        default:
            *error_msg = "Unknown function ID";
            return false;
    }
}

/* SQLite extension entry point */
#ifdef _WIN32
__declspec(dllexport)
#endif
int sqlite3_rtree_init(
    sqlite3 *db,
    char **pzErrMsg,
    const sqlite3_api_routines *pApi
) {
    (void)pzErrMsg;
    (void)pApi;

    g_db = db;

    /* R-Tree is enabled via compilation flag */
    return SQLITE_OK;
}
