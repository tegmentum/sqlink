/*
 * FTS5 Extension for SQLite WASM
 *
 * This extension provides full-text search capabilities as a
 * dynamically loadable WASM component.
 */

#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include "sqlite3.h"

/* Extension metadata */
#define EXT_NAME "fts5"
#define EXT_VERSION "1.0.0"

/* Function IDs for FTS5 functions */
#define FUNC_FTS5 1
#define FUNC_FTS5_SOURCE_ID 2
#define FUNC_HIGHLIGHT 3
#define FUNC_SNIPPET 4
#define FUNC_BM25 5

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

/* FTS5 functions list */
static function_def_t g_functions[] = {
    { "fts5", -1, FUNC_FTS5 },
    { "fts5_source_id", 0, FUNC_FTS5_SOURCE_ID },
    { "highlight", 4, FUNC_HIGHLIGHT },
    { "snippet", 6, FUNC_SNIPPET },
    { "bm25", -1, FUNC_BM25 },
};

/* Export: get-info */
void exports_get_info(extension_info_t *info) {
    info->name = EXT_NAME;
    info->version = EXT_VERSION;
    info->functions = g_functions;
    info->function_count = sizeof(g_functions) / sizeof(g_functions[0]);
}

/* Export: init - Initialize FTS5 extension */
bool exports_init(uint64_t db_handle, char **error_msg) {
    g_db = (sqlite3 *)(uintptr_t)db_handle;

    if (g_db == NULL) {
        *error_msg = "Invalid database handle";
        return false;
    }

    /* FTS5 is initialized by SQLite when compiled with SQLITE_ENABLE_FTS5 */
    /* The extension init function is called automatically by sqlite3_auto_extension */

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

/* Export: on-function-call - Handle function invocations
 * Note: In a full implementation, this would dispatch to the actual
 * FTS5 functions. For now, it's a placeholder that the host-side
 * extension loader can use to route calls.
 */
bool exports_on_function_call(
    uint64_t function_id,
    sql_value_t *args,
    size_t arg_count,
    sql_value_t *result,
    char **error_msg
) {
    switch (function_id) {
        case FUNC_FTS5_SOURCE_ID:
            /* Return the FTS5 source ID */
            result->type = VALUE_TYPE_TEXT;
            result->text_value = "fts5-wasm-extension";
            result->text_len = strlen(result->text_value);
            return true;

        case FUNC_FTS5:
        case FUNC_HIGHLIGHT:
        case FUNC_SNIPPET:
        case FUNC_BM25:
            /* These are virtual table functions that work within FTS5 context */
            /* The actual implementation is handled by SQLite's FTS5 module */
            *error_msg = "Function must be called within FTS5 context";
            return false;

        default:
            *error_msg = "Unknown function ID";
            return false;
    }
}

/* SQLite extension entry point
 * Called when the extension is loaded via sqlite3_load_extension
 */
#ifdef _WIN32
__declspec(dllexport)
#endif
int sqlite3_fts5_init(
    sqlite3 *db,
    char **pzErrMsg,
    const sqlite3_api_routines *pApi
) {
    (void)pzErrMsg;
    (void)pApi;

    g_db = db;

    /* FTS5 is enabled via compilation flag, so this just marks
     * the extension as successfully loaded */
    return SQLITE_OK;
}
