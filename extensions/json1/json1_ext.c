/*
 * JSON1 Extension for SQLite WASM
 *
 * This extension provides JSON manipulation functions as a
 * dynamically loadable WASM component.
 */

#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include "sqlite3.h"

/* Extension metadata */
#define EXT_NAME "json1"
#define EXT_VERSION "1.0.0"

/* Function IDs for JSON functions */
#define FUNC_JSON 1
#define FUNC_JSON_ARRAY 2
#define FUNC_JSON_ARRAY_LENGTH 3
#define FUNC_JSON_EXTRACT 4
#define FUNC_JSON_INSERT 5
#define FUNC_JSON_OBJECT 6
#define FUNC_JSON_PATCH 7
#define FUNC_JSON_REMOVE 8
#define FUNC_JSON_REPLACE 9
#define FUNC_JSON_SET 10
#define FUNC_JSON_TYPE 11
#define FUNC_JSON_VALID 12
#define FUNC_JSON_QUOTE 13
#define FUNC_JSON_GROUP_ARRAY 14
#define FUNC_JSON_GROUP_OBJECT 15
#define FUNC_JSON_EACH 16
#define FUNC_JSON_TREE 17

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

/* JSON1 functions list */
static function_def_t g_functions[] = {
    { "json", 1, FUNC_JSON },
    { "json_array", -1, FUNC_JSON_ARRAY },
    { "json_array_length", -1, FUNC_JSON_ARRAY_LENGTH },
    { "json_extract", -1, FUNC_JSON_EXTRACT },
    { "json_insert", -1, FUNC_JSON_INSERT },
    { "json_object", -1, FUNC_JSON_OBJECT },
    { "json_patch", 2, FUNC_JSON_PATCH },
    { "json_remove", -1, FUNC_JSON_REMOVE },
    { "json_replace", -1, FUNC_JSON_REPLACE },
    { "json_set", -1, FUNC_JSON_SET },
    { "json_type", -1, FUNC_JSON_TYPE },
    { "json_valid", 1, FUNC_JSON_VALID },
    { "json_quote", 1, FUNC_JSON_QUOTE },
    { "json_group_array", 1, FUNC_JSON_GROUP_ARRAY },
    { "json_group_object", 2, FUNC_JSON_GROUP_OBJECT },
    { "json_each", -1, FUNC_JSON_EACH },
    { "json_tree", -1, FUNC_JSON_TREE },
};

/* Export: get-info */
void exports_get_info(extension_info_t *info) {
    info->name = EXT_NAME;
    info->version = EXT_VERSION;
    info->functions = g_functions;
    info->function_count = sizeof(g_functions) / sizeof(g_functions[0]);
}

/* Export: init - Initialize JSON1 extension */
bool exports_init(uint64_t db_handle, char **error_msg) {
    g_db = (sqlite3 *)(uintptr_t)db_handle;

    if (g_db == NULL) {
        *error_msg = "Invalid database handle";
        return false;
    }

    /* JSON1 is initialized by SQLite when compiled with SQLITE_ENABLE_JSON1 */
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
        case FUNC_JSON_VALID:
            /* For now, a simple placeholder */
            result->type = VALUE_TYPE_INTEGER;
            result->int_value = 1;
            return true;

        case FUNC_JSON:
        case FUNC_JSON_ARRAY:
        case FUNC_JSON_ARRAY_LENGTH:
        case FUNC_JSON_EXTRACT:
        case FUNC_JSON_INSERT:
        case FUNC_JSON_OBJECT:
        case FUNC_JSON_PATCH:
        case FUNC_JSON_REMOVE:
        case FUNC_JSON_REPLACE:
        case FUNC_JSON_SET:
        case FUNC_JSON_TYPE:
        case FUNC_JSON_QUOTE:
        case FUNC_JSON_GROUP_ARRAY:
        case FUNC_JSON_GROUP_OBJECT:
        case FUNC_JSON_EACH:
        case FUNC_JSON_TREE:
            /* These functions are implemented by SQLite's JSON1 module */
            *error_msg = "Function handled by SQLite JSON1 module";
            return false;

        default:
            *error_msg = "Unknown function ID";
            return false;
    }
}

/* SQLite extension entry point */
#ifdef _WIN32
__declspec(dllexport)
#endif
int sqlite3_json1_init(
    sqlite3 *db,
    char **pzErrMsg,
    const sqlite3_api_routines *pApi
) {
    (void)pzErrMsg;
    (void)pApi;

    g_db = db;

    /* JSON1 is enabled via compilation flag */
    return SQLITE_OK;
}
