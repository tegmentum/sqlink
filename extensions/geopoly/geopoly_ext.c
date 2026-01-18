/*
 * GeoPoly Extension for SQLite WASM
 *
 * This extension provides polygon geometry functions as a
 * dynamically loadable WASM component. It builds on R-Tree.
 */

#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include "sqlite3.h"

/* Extension metadata */
#define EXT_NAME "geopoly"
#define EXT_VERSION "1.0.0"

/* Function IDs for GeoPoly functions */
#define FUNC_GEOPOLY 1
#define FUNC_GEOPOLY_AREA 2
#define FUNC_GEOPOLY_BLOB 3
#define FUNC_GEOPOLY_JSON 4
#define FUNC_GEOPOLY_SVG 5
#define FUNC_GEOPOLY_CONTAINS_POINT 6
#define FUNC_GEOPOLY_WITHIN 7
#define FUNC_GEOPOLY_OVERLAP 8
#define FUNC_GEOPOLY_REGULAR 9
#define FUNC_GEOPOLY_BBOX 10
#define FUNC_GEOPOLY_XFORM 11
#define FUNC_GEOPOLY_CCW 12

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

/* GeoPoly functions list */
static function_def_t g_functions[] = {
    { "geopoly", -1, FUNC_GEOPOLY },
    { "geopoly_area", 1, FUNC_GEOPOLY_AREA },
    { "geopoly_blob", 1, FUNC_GEOPOLY_BLOB },
    { "geopoly_json", 1, FUNC_GEOPOLY_JSON },
    { "geopoly_svg", -1, FUNC_GEOPOLY_SVG },
    { "geopoly_contains_point", 3, FUNC_GEOPOLY_CONTAINS_POINT },
    { "geopoly_within", 2, FUNC_GEOPOLY_WITHIN },
    { "geopoly_overlap", 2, FUNC_GEOPOLY_OVERLAP },
    { "geopoly_regular", 4, FUNC_GEOPOLY_REGULAR },
    { "geopoly_bbox", 1, FUNC_GEOPOLY_BBOX },
    { "geopoly_xform", 7, FUNC_GEOPOLY_XFORM },
    { "geopoly_ccw", 1, FUNC_GEOPOLY_CCW },
};

/* Export: get-info */
void exports_get_info(extension_info_t *info) {
    info->name = EXT_NAME;
    info->version = EXT_VERSION;
    info->functions = g_functions;
    info->function_count = sizeof(g_functions) / sizeof(g_functions[0]);
}

/* Export: init - Initialize GeoPoly extension */
bool exports_init(uint64_t db_handle, char **error_msg) {
    g_db = (sqlite3 *)(uintptr_t)db_handle;

    if (g_db == NULL) {
        *error_msg = "Invalid database handle";
        return false;
    }

    /* GeoPoly is initialized by SQLite when compiled with SQLITE_ENABLE_GEOPOLY */
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
    (void)result;

    switch (function_id) {
        case FUNC_GEOPOLY:
        case FUNC_GEOPOLY_AREA:
        case FUNC_GEOPOLY_BLOB:
        case FUNC_GEOPOLY_JSON:
        case FUNC_GEOPOLY_SVG:
        case FUNC_GEOPOLY_CONTAINS_POINT:
        case FUNC_GEOPOLY_WITHIN:
        case FUNC_GEOPOLY_OVERLAP:
        case FUNC_GEOPOLY_REGULAR:
        case FUNC_GEOPOLY_BBOX:
        case FUNC_GEOPOLY_XFORM:
        case FUNC_GEOPOLY_CCW:
            /* These functions are implemented by SQLite's GeoPoly module */
            *error_msg = "Function handled by SQLite GeoPoly module";
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
int sqlite3_geopoly_init(
    sqlite3 *db,
    char **pzErrMsg,
    const sqlite3_api_routines *pApi
) {
    (void)pzErrMsg;
    (void)pApi;

    g_db = db;

    /* GeoPoly is enabled via compilation flag */
    return SQLITE_OK;
}
