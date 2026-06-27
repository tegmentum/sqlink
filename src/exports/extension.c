/*
 * SQLite WASM Extension API Implementation
 *
 * This file provides the implementation of the extension API
 * for custom SQL functions, collations, and hooks.
 */

#include <stdlib.h>
#include <string.h>
#include "sqlite3.h"
#include "../bindings-ext/sqlite_extensible.h"

/*
 * Handle to pointer conversion (same as low_level.c)
 */
#define PTR_TO_HANDLE(ptr) ((uint64_t)(uintptr_t)(ptr))
#define HANDLE_TO_DB(h) ((sqlite3*)(uintptr_t)(h))

/*
 * Registration tracking structures
 */

typedef struct {
    uint64_t function_id;
    sqlite3 *db;
    char *name;
    int num_args;
    bool is_aggregate;
} FunctionRegistration;

typedef struct {
    uint64_t collation_id;
    sqlite3 *db;
    char *name;
} CollationRegistration;

typedef struct {
    uint64_t hook_id;
    sqlite3 *db;
    int type;  /* 0=update, 1=commit, 2=rollback, 3=authorizer */
} HookRegistration;

/* Simple dynamic arrays for tracking registrations */
#define MAX_REGISTRATIONS 256
static FunctionRegistration g_functions[MAX_REGISTRATIONS];
static int g_function_count = 0;

static CollationRegistration g_collations[MAX_REGISTRATIONS];
static int g_collation_count = 0;

static HookRegistration g_hooks[MAX_REGISTRATIONS];
static int g_hook_count = 0;

/* Counter for generating unique aggregate context IDs */
static uint64_t g_aggregate_context_counter = 1;

/*
 * Helper to create sql-value from sqlite3_value
 * Uses the callback types since these are passed to imported callback functions
 */
static void sqlite_value_to_wit(sqlite3_value *val, sqlink_wasm_extension_callbacks_sql_value_t *out) {
    int type = sqlite3_value_type(val);

    out->int_value.is_some = false;
    out->float_value.is_some = false;
    out->text_value.is_some = false;
    out->blob_value.is_some = false;

    switch (type) {
        case SQLITE_INTEGER:
            out->value_type = SQLINK_WASM_EXTENSION_VALUE_TYPE_INTEGER;
            out->int_value.is_some = true;
            out->int_value.val = sqlite3_value_int64(val);
            break;
        case SQLITE_FLOAT:
            out->value_type = SQLINK_WASM_EXTENSION_VALUE_TYPE_FLOAT;
            out->float_value.is_some = true;
            out->float_value.val = sqlite3_value_double(val);
            break;
        case SQLITE_TEXT:
            out->value_type = SQLINK_WASM_EXTENSION_VALUE_TYPE_TEXT;
            out->text_value.is_some = true;
            {
                const char *text = (const char *)sqlite3_value_text(val);
                int len = sqlite3_value_bytes(val);
                out->text_value.val.ptr = (uint8_t *)malloc(len);
                if (out->text_value.val.ptr) {
                    memcpy(out->text_value.val.ptr, text, len);
                    out->text_value.val.len = len;
                } else {
                    out->text_value.val.len = 0;
                }
            }
            break;
        case SQLITE_BLOB:
            out->value_type = SQLINK_WASM_EXTENSION_VALUE_TYPE_BLOB;
            out->blob_value.is_some = true;
            {
                const void *blob = sqlite3_value_blob(val);
                int len = sqlite3_value_bytes(val);
                out->blob_value.val.ptr = (uint8_t *)malloc(len);
                if (out->blob_value.val.ptr) {
                    memcpy(out->blob_value.val.ptr, blob, len);
                    out->blob_value.val.len = len;
                } else {
                    out->blob_value.val.len = 0;
                }
            }
            break;
        case SQLITE_NULL:
        default:
            out->value_type = SQLINK_WASM_EXTENSION_VALUE_TYPE_NULL;
            break;
    }
}

/*
 * Helper to set sqlite3_context result from wit sql-value
 * Uses the callback types since these come from imported callback functions
 */
static void wit_value_to_result(sqlite3_context *ctx, sqlink_wasm_extension_callbacks_sql_value_t *val) {
    switch (val->value_type) {
        case SQLINK_WASM_EXTENSION_VALUE_TYPE_INTEGER:
            if (val->int_value.is_some) {
                sqlite3_result_int64(ctx, val->int_value.val);
            } else {
                sqlite3_result_null(ctx);
            }
            break;
        case SQLINK_WASM_EXTENSION_VALUE_TYPE_FLOAT:
            if (val->float_value.is_some) {
                sqlite3_result_double(ctx, val->float_value.val);
            } else {
                sqlite3_result_null(ctx);
            }
            break;
        case SQLINK_WASM_EXTENSION_VALUE_TYPE_TEXT:
            if (val->text_value.is_some) {
                sqlite3_result_text(ctx, (const char *)val->text_value.val.ptr,
                    (int)val->text_value.val.len, SQLITE_TRANSIENT);
            } else {
                sqlite3_result_null(ctx);
            }
            break;
        case SQLINK_WASM_EXTENSION_VALUE_TYPE_BLOB:
            if (val->blob_value.is_some) {
                sqlite3_result_blob(ctx, val->blob_value.val.ptr,
                    (int)val->blob_value.val.len, SQLITE_TRANSIENT);
            } else {
                sqlite3_result_null(ctx);
            }
            break;
        case SQLINK_WASM_EXTENSION_VALUE_TYPE_NULL:
        default:
            sqlite3_result_null(ctx);
            break;
    }
}

/*
 * Scalar function callback
 */
static void scalar_function_callback(sqlite3_context *ctx, int argc, sqlite3_value **argv) {
    uint64_t function_id = (uint64_t)(uintptr_t)sqlite3_user_data(ctx);

    /* Build args list */
    sqlink_wasm_extension_callbacks_list_sql_value_t args;
    args.len = argc;
    args.ptr = (sqlink_wasm_extension_callbacks_sql_value_t *)
        malloc(argc * sizeof(sqlink_wasm_extension_callbacks_sql_value_t));

    if (args.ptr) {
        for (int i = 0; i < argc; i++) {
            sqlite_value_to_wit(argv[i], &args.ptr[i]);
        }
    } else {
        args.len = 0;
    }

    /* Call the imported callback */
    sqlink_wasm_extension_callbacks_sql_value_t result_val;
    sqlite_extensible_string_t result_err;
    bool success = sqlink_wasm_extension_callbacks_on_scalar_function(function_id, &args, &result_val, &result_err);

    /* Handle result */
    if (!success) {
        sqlite3_result_error(ctx, (const char *)result_err.ptr, (int)result_err.len);
    } else {
        wit_value_to_result(ctx, &result_val);
    }

    /* Clean up args */
    for (size_t i = 0; i < args.len; i++) {
        if (args.ptr[i].text_value.is_some && args.ptr[i].text_value.val.ptr) {
            free(args.ptr[i].text_value.val.ptr);
        }
        if (args.ptr[i].blob_value.is_some && args.ptr[i].blob_value.val.ptr) {
            free(args.ptr[i].blob_value.val.ptr);
        }
    }
    if (args.ptr) free(args.ptr);
}

/*
 * Aggregate step callback
 */
static void aggregate_step_callback(sqlite3_context *ctx, int argc, sqlite3_value **argv) {
    uint64_t function_id = (uint64_t)(uintptr_t)sqlite3_user_data(ctx);

    /* Get or create aggregate context */
    uint64_t *agg_ctx = (uint64_t *)sqlite3_aggregate_context(ctx, sizeof(uint64_t));
    if (!agg_ctx) return;

    if (*agg_ctx == 0) {
        *agg_ctx = g_aggregate_context_counter++;
    }

    /* Build args list */
    sqlink_wasm_extension_callbacks_list_sql_value_t args;
    args.len = argc;
    args.ptr = (sqlink_wasm_extension_callbacks_sql_value_t *)
        malloc(argc * sizeof(sqlink_wasm_extension_callbacks_sql_value_t));

    if (args.ptr) {
        for (int i = 0; i < argc; i++) {
            sqlite_value_to_wit(argv[i], &args.ptr[i]);
        }
    } else {
        args.len = 0;
    }

    /* Call the imported callback */
    sqlink_wasm_extension_callbacks_on_aggregate_step(function_id, *agg_ctx, &args);

    /* Clean up args */
    for (size_t i = 0; i < args.len; i++) {
        if (args.ptr[i].text_value.is_some && args.ptr[i].text_value.val.ptr) {
            free(args.ptr[i].text_value.val.ptr);
        }
        if (args.ptr[i].blob_value.is_some && args.ptr[i].blob_value.val.ptr) {
            free(args.ptr[i].blob_value.val.ptr);
        }
    }
    if (args.ptr) free(args.ptr);
}

/*
 * Aggregate finalize callback
 */
static void aggregate_finalize_callback(sqlite3_context *ctx) {
    uint64_t function_id = (uint64_t)(uintptr_t)sqlite3_user_data(ctx);

    /* Get aggregate context */
    uint64_t *agg_ctx = (uint64_t *)sqlite3_aggregate_context(ctx, 0);
    uint64_t context_id = agg_ctx ? *agg_ctx : 0;

    /* Call the imported callback */
    sqlink_wasm_extension_callbacks_sql_value_t result_val;
    sqlite_extensible_string_t result_err;
    bool success = sqlink_wasm_extension_callbacks_on_aggregate_finalize(function_id, context_id, &result_val, &result_err);

    /* Handle result */
    if (!success) {
        sqlite3_result_error(ctx, (const char *)result_err.ptr, (int)result_err.len);
    } else {
        wit_value_to_result(ctx, &result_val);
    }
}

/*
 * Collation comparison callback
 */
static int collation_compare_callback(void *user_data, int len1, const void *str1,
                                       int len2, const void *str2) {
    uint64_t collation_id = (uint64_t)(uintptr_t)user_data;

    sqlite_extensible_string_t a, b;
    a.ptr = (uint8_t *)str1;
    a.len = len1;
    b.ptr = (uint8_t *)str2;
    b.len = len2;

    return sqlink_wasm_extension_callbacks_on_collation_compare(collation_id, &a, &b);
}

/*
 * Update hook callback
 */
static void update_hook_callback(void *user_data, int op, const char *database,
                                  const char *table, sqlite3_int64 rowid) {
    uint64_t hook_id = (uint64_t)(uintptr_t)user_data;

    sqlink_wasm_extension_callbacks_update_type_t update_type;
    switch (op) {
        case SQLITE_INSERT:
            update_type = SQLINK_WASM_EXTENSION_UPDATE_TYPE_INSERT;
            break;
        case SQLITE_UPDATE:
            update_type = SQLINK_WASM_EXTENSION_UPDATE_TYPE_UPDATE;
            break;
        case SQLITE_DELETE:
            update_type = SQLINK_WASM_EXTENSION_UPDATE_TYPE_DELETE;
            break;
        default:
            return;
    }

    sqlite_extensible_string_t db_str, table_str;
    db_str.ptr = (uint8_t *)database;
    db_str.len = database ? strlen(database) : 0;
    table_str.ptr = (uint8_t *)table;
    table_str.len = table ? strlen(table) : 0;

    sqlink_wasm_extension_callbacks_on_update(hook_id, update_type, &db_str, &table_str, rowid);
}

/*
 * Commit hook callback
 */
static int commit_hook_callback(void *user_data) {
    uint64_t hook_id = (uint64_t)(uintptr_t)user_data;
    return sqlink_wasm_extension_callbacks_on_commit(hook_id) ? 1 : 0;
}

/*
 * Rollback hook callback
 */
static void rollback_hook_callback(void *user_data) {
    uint64_t hook_id = (uint64_t)(uintptr_t)user_data;
    sqlink_wasm_extension_callbacks_on_rollback(hook_id);
}

/*
 * Authorizer callback
 */
static int authorizer_callback(void *user_data, int action, const char *arg1,
                                const char *arg2, const char *database, const char *trigger) {
    uint64_t auth_id = (uint64_t)(uintptr_t)user_data;

    sqlink_wasm_extension_callbacks_auth_action_t wit_action;
    switch (action) {
        case SQLITE_CREATE_INDEX: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_CREATE_INDEX; break;
        case SQLITE_CREATE_TABLE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_CREATE_TABLE; break;
        case SQLITE_CREATE_TEMP_INDEX: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_CREATE_TEMP_INDEX; break;
        case SQLITE_CREATE_TEMP_TABLE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_CREATE_TEMP_TABLE; break;
        case SQLITE_CREATE_TEMP_TRIGGER: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_CREATE_TEMP_TRIGGER; break;
        case SQLITE_CREATE_TEMP_VIEW: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_CREATE_TEMP_VIEW; break;
        case SQLITE_CREATE_TRIGGER: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_CREATE_TRIGGER; break;
        case SQLITE_CREATE_VIEW: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_CREATE_VIEW; break;
        case SQLITE_DELETE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DELETE; break;
        case SQLITE_DROP_INDEX: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DROP_INDEX; break;
        case SQLITE_DROP_TABLE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DROP_TABLE; break;
        case SQLITE_DROP_TEMP_INDEX: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DROP_TEMP_INDEX; break;
        case SQLITE_DROP_TEMP_TABLE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DROP_TEMP_TABLE; break;
        case SQLITE_DROP_TEMP_TRIGGER: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DROP_TEMP_TRIGGER; break;
        case SQLITE_DROP_TEMP_VIEW: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DROP_TEMP_VIEW; break;
        case SQLITE_DROP_TRIGGER: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DROP_TRIGGER; break;
        case SQLITE_DROP_VIEW: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DROP_VIEW; break;
        case SQLITE_INSERT: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_INSERT; break;
        case SQLITE_PRAGMA: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_PRAGMA; break;
        case SQLITE_READ: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_READ; break;
        case SQLITE_SELECT: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_SELECT; break;
        case SQLITE_TRANSACTION: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_TRANSACTION; break;
        case SQLITE_UPDATE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_UPDATE; break;
        case SQLITE_ATTACH: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_ATTACH; break;
        case SQLITE_DETACH: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DETACH; break;
        case SQLITE_ALTER_TABLE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_ALTER_TABLE; break;
        case SQLITE_REINDEX: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_REINDEX; break;
        case SQLITE_ANALYZE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_ANALYZE; break;
        case SQLITE_CREATE_VTABLE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_CREATE_VTABLE; break;
        case SQLITE_DROP_VTABLE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_DROP_VTABLE; break;
        case SQLITE_FUNCTION: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_FUNCTION; break;
        case SQLITE_SAVEPOINT: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_SAVEPOINT; break;
        case SQLITE_RECURSIVE: wit_action = SQLINK_WASM_EXTENSION_AUTH_ACTION_RECURSIVE; break;
        default: return SQLITE_OK;
    }

    /* Build strings - the callback takes nullable string pointers */
    sqlite_extensible_string_t str_arg1, str_arg2, str_db, str_trigger;

    if (arg1) {
        str_arg1.ptr = (uint8_t *)arg1;
        str_arg1.len = strlen(arg1);
    }

    if (arg2) {
        str_arg2.ptr = (uint8_t *)arg2;
        str_arg2.len = strlen(arg2);
    }

    if (database) {
        str_db.ptr = (uint8_t *)database;
        str_db.len = strlen(database);
    }

    if (trigger) {
        str_trigger.ptr = (uint8_t *)trigger;
        str_trigger.len = strlen(trigger);
    }

    sqlink_wasm_extension_callbacks_auth_result_t result =
        sqlink_wasm_extension_callbacks_on_authorize(auth_id, wit_action,
            arg1 ? &str_arg1 : NULL,
            arg2 ? &str_arg2 : NULL,
            database ? &str_db : NULL,
            trigger ? &str_trigger : NULL);

    switch (result) {
        case SQLINK_WASM_EXTENSION_AUTH_RESULT_OK: return SQLITE_OK;
        case SQLINK_WASM_EXTENSION_AUTH_RESULT_DENY: return SQLITE_DENY;
        case SQLINK_WASM_EXTENSION_AUTH_RESULT_IGNORE: return SQLITE_IGNORE;
        default: return SQLITE_OK;
    }
}

/*
 * Helper to copy string from wit
 */
static char *wit_string_to_cstr(sqlite_extensible_string_t *str) {
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
 * Helper to set extension error
 */
static void set_extension_error(exports_sqlink_wasm_extension_extension_error_t *err,
                                 int code, const char *msg) {
    err->code = code;
    if (msg) {
        size_t len = strlen(msg);
        err->message.ptr = (uint8_t *)malloc(len);
        if (err->message.ptr) {
            memcpy(err->message.ptr, msg, len);
            err->message.len = len;
        } else {
            err->message.len = 0;
        }
    } else {
        err->message.ptr = NULL;
        err->message.len = 0;
    }
}

/*
 * Convert function flags
 */
static int wit_to_sqlite_function_flags(exports_sqlink_wasm_extension_function_flags_t flags) {
    int sqlite_flags = SQLITE_UTF8;
    if (flags & EXPORTS_SQLINK_WASM_EXTENSION_FUNCTION_FLAGS_DETERMINISTIC) {
        sqlite_flags |= SQLITE_DETERMINISTIC;
    }
    if (flags & EXPORTS_SQLINK_WASM_EXTENSION_FUNCTION_FLAGS_DIRECT_ONLY) {
        sqlite_flags |= SQLITE_DIRECTONLY;
    }
    return sqlite_flags;
}

/*
 * Extension API Implementation
 */

bool exports_sqlink_wasm_extension_register_scalar_function(
    exports_sqlink_wasm_extension_db_handle_t db,
    sqlite_extensible_string_t *name,
    int32_t num_args,
    exports_sqlink_wasm_extension_function_flags_t flags,
    uint64_t function_id,
    exports_sqlink_wasm_extension_function_handle_t *ret,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    char *func_name = wit_string_to_cstr(name);
    if (!func_name) {
        set_extension_error(err, SQLITE_ERROR, "Invalid function name");
        return false;
    }

    int sqlite_flags = wit_to_sqlite_function_flags(flags);

    int rc = sqlite3_create_function_v2(
        pdb,
        func_name,
        num_args,
        sqlite_flags,
        (void *)(uintptr_t)function_id,
        scalar_function_callback,
        NULL,
        NULL,
        NULL
    );

    if (rc != SQLITE_OK) {
        set_extension_error(err, rc, sqlite3_errmsg(pdb));
        free(func_name);
        return false;
    }

    /* Track registration */
    if (g_function_count < MAX_REGISTRATIONS) {
        g_functions[g_function_count].function_id = function_id;
        g_functions[g_function_count].db = pdb;
        g_functions[g_function_count].name = func_name;
        g_functions[g_function_count].num_args = num_args;
        g_functions[g_function_count].is_aggregate = false;
        *ret = (uint64_t)g_function_count;
        g_function_count++;
    } else {
        free(func_name);
        *ret = 0;
    }

    return true;
}

bool exports_sqlink_wasm_extension_unregister_function(
    exports_sqlink_wasm_extension_function_handle_t handle,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    if (handle >= (uint64_t)g_function_count) {
        set_extension_error(err, SQLITE_ERROR, "Invalid function handle");
        return false;
    }

    FunctionRegistration *reg = &g_functions[handle];
    if (!reg->db || !reg->name) {
        set_extension_error(err, SQLITE_ERROR, "Function already unregistered");
        return false;
    }

    /* Remove function by registering NULL */
    int rc = sqlite3_create_function_v2(
        reg->db,
        reg->name,
        reg->num_args,
        SQLITE_UTF8,
        NULL,
        NULL,
        NULL,
        NULL,
        NULL
    );

    if (rc != SQLITE_OK) {
        set_extension_error(err, rc, sqlite3_errmsg(reg->db));
        return false;
    }

    free(reg->name);
    reg->name = NULL;
    reg->db = NULL;

    return true;
}

bool exports_sqlink_wasm_extension_register_aggregate_function(
    exports_sqlink_wasm_extension_db_handle_t db,
    sqlite_extensible_string_t *name,
    int32_t num_args,
    exports_sqlink_wasm_extension_function_flags_t flags,
    uint64_t function_id,
    exports_sqlink_wasm_extension_function_handle_t *ret,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    char *func_name = wit_string_to_cstr(name);
    if (!func_name) {
        set_extension_error(err, SQLITE_ERROR, "Invalid function name");
        return false;
    }

    int sqlite_flags = wit_to_sqlite_function_flags(flags);

    int rc = sqlite3_create_function_v2(
        pdb,
        func_name,
        num_args,
        sqlite_flags,
        (void *)(uintptr_t)function_id,
        NULL,
        aggregate_step_callback,
        aggregate_finalize_callback,
        NULL
    );

    if (rc != SQLITE_OK) {
        set_extension_error(err, rc, sqlite3_errmsg(pdb));
        free(func_name);
        return false;
    }

    /* Track registration */
    if (g_function_count < MAX_REGISTRATIONS) {
        g_functions[g_function_count].function_id = function_id;
        g_functions[g_function_count].db = pdb;
        g_functions[g_function_count].name = func_name;
        g_functions[g_function_count].num_args = num_args;
        g_functions[g_function_count].is_aggregate = true;
        *ret = (uint64_t)g_function_count;
        g_function_count++;
    } else {
        free(func_name);
        *ret = 0;
    }

    return true;
}

bool exports_sqlink_wasm_extension_register_collation(
    exports_sqlink_wasm_extension_db_handle_t db,
    sqlite_extensible_string_t *name,
    uint64_t collation_id,
    exports_sqlink_wasm_extension_collation_handle_t *ret,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);
    char *coll_name = wit_string_to_cstr(name);
    if (!coll_name) {
        set_extension_error(err, SQLITE_ERROR, "Invalid collation name");
        return false;
    }

    int rc = sqlite3_create_collation_v2(
        pdb,
        coll_name,
        SQLITE_UTF8,
        (void *)(uintptr_t)collation_id,
        collation_compare_callback,
        NULL
    );

    if (rc != SQLITE_OK) {
        set_extension_error(err, rc, sqlite3_errmsg(pdb));
        free(coll_name);
        return false;
    }

    /* Track registration */
    if (g_collation_count < MAX_REGISTRATIONS) {
        g_collations[g_collation_count].collation_id = collation_id;
        g_collations[g_collation_count].db = pdb;
        g_collations[g_collation_count].name = coll_name;
        *ret = (uint64_t)g_collation_count;
        g_collation_count++;
    } else {
        free(coll_name);
        *ret = 0;
    }

    return true;
}

bool exports_sqlink_wasm_extension_unregister_collation(
    exports_sqlink_wasm_extension_collation_handle_t handle,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    if (handle >= (uint64_t)g_collation_count) {
        set_extension_error(err, SQLITE_ERROR, "Invalid collation handle");
        return false;
    }

    CollationRegistration *reg = &g_collations[handle];
    if (!reg->db || !reg->name) {
        set_extension_error(err, SQLITE_ERROR, "Collation already unregistered");
        return false;
    }

    /* Remove collation by registering NULL */
    int rc = sqlite3_create_collation_v2(
        reg->db,
        reg->name,
        SQLITE_UTF8,
        NULL,
        NULL,
        NULL
    );

    if (rc != SQLITE_OK) {
        set_extension_error(err, rc, sqlite3_errmsg(reg->db));
        return false;
    }

    free(reg->name);
    reg->name = NULL;
    reg->db = NULL;

    return true;
}

bool exports_sqlink_wasm_extension_set_update_hook(
    exports_sqlink_wasm_extension_db_handle_t db,
    uint64_t hook_id,
    exports_sqlink_wasm_extension_hook_handle_t *ret,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);

    sqlite3_update_hook(pdb, update_hook_callback, (void *)(uintptr_t)hook_id);

    /* Track registration */
    if (g_hook_count < MAX_REGISTRATIONS) {
        g_hooks[g_hook_count].hook_id = hook_id;
        g_hooks[g_hook_count].db = pdb;
        g_hooks[g_hook_count].type = 0;
        *ret = (uint64_t)g_hook_count;
        g_hook_count++;
    } else {
        *ret = 0;
    }

    return true;
}

bool exports_sqlink_wasm_extension_remove_update_hook(
    exports_sqlink_wasm_extension_hook_handle_t handle,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    if (handle >= (uint64_t)g_hook_count) {
        set_extension_error(err, SQLITE_ERROR, "Invalid hook handle");
        return false;
    }

    HookRegistration *reg = &g_hooks[handle];
    if (reg->db) {
        sqlite3_update_hook(reg->db, NULL, NULL);
        reg->db = NULL;
    }

    return true;
}

bool exports_sqlink_wasm_extension_set_commit_hook(
    exports_sqlink_wasm_extension_db_handle_t db,
    uint64_t hook_id,
    exports_sqlink_wasm_extension_hook_handle_t *ret,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);

    sqlite3_commit_hook(pdb, commit_hook_callback, (void *)(uintptr_t)hook_id);

    /* Track registration */
    if (g_hook_count < MAX_REGISTRATIONS) {
        g_hooks[g_hook_count].hook_id = hook_id;
        g_hooks[g_hook_count].db = pdb;
        g_hooks[g_hook_count].type = 1;
        *ret = (uint64_t)g_hook_count;
        g_hook_count++;
    } else {
        *ret = 0;
    }

    return true;
}

bool exports_sqlink_wasm_extension_remove_commit_hook(
    exports_sqlink_wasm_extension_hook_handle_t handle,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    if (handle >= (uint64_t)g_hook_count) {
        set_extension_error(err, SQLITE_ERROR, "Invalid hook handle");
        return false;
    }

    HookRegistration *reg = &g_hooks[handle];
    if (reg->db) {
        sqlite3_commit_hook(reg->db, NULL, NULL);
        reg->db = NULL;
    }

    return true;
}

bool exports_sqlink_wasm_extension_set_rollback_hook(
    exports_sqlink_wasm_extension_db_handle_t db,
    uint64_t hook_id,
    exports_sqlink_wasm_extension_hook_handle_t *ret,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);

    sqlite3_rollback_hook(pdb, rollback_hook_callback, (void *)(uintptr_t)hook_id);

    /* Track registration */
    if (g_hook_count < MAX_REGISTRATIONS) {
        g_hooks[g_hook_count].hook_id = hook_id;
        g_hooks[g_hook_count].db = pdb;
        g_hooks[g_hook_count].type = 2;
        *ret = (uint64_t)g_hook_count;
        g_hook_count++;
    } else {
        *ret = 0;
    }

    return true;
}

bool exports_sqlink_wasm_extension_remove_rollback_hook(
    exports_sqlink_wasm_extension_hook_handle_t handle,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    if (handle >= (uint64_t)g_hook_count) {
        set_extension_error(err, SQLITE_ERROR, "Invalid hook handle");
        return false;
    }

    HookRegistration *reg = &g_hooks[handle];
    if (reg->db) {
        sqlite3_rollback_hook(reg->db, NULL, NULL);
        reg->db = NULL;
    }

    return true;
}

bool exports_sqlink_wasm_extension_set_busy_timeout(
    exports_sqlink_wasm_extension_db_handle_t db,
    int32_t ms,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);

    int rc = sqlite3_busy_timeout(pdb, ms);
    if (rc != SQLITE_OK) {
        set_extension_error(err, rc, sqlite3_errmsg(pdb));
        return false;
    }

    return true;
}

bool exports_sqlink_wasm_extension_set_authorizer(
    exports_sqlink_wasm_extension_db_handle_t db,
    uint64_t auth_id,
    exports_sqlink_wasm_extension_hook_handle_t *ret,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    sqlite3 *pdb = HANDLE_TO_DB(db);

    int rc = sqlite3_set_authorizer(pdb, authorizer_callback, (void *)(uintptr_t)auth_id);
    if (rc != SQLITE_OK) {
        set_extension_error(err, rc, sqlite3_errmsg(pdb));
        return false;
    }

    /* Track registration */
    if (g_hook_count < MAX_REGISTRATIONS) {
        g_hooks[g_hook_count].hook_id = auth_id;
        g_hooks[g_hook_count].db = pdb;
        g_hooks[g_hook_count].type = 3;
        *ret = (uint64_t)g_hook_count;
        g_hook_count++;
    } else {
        *ret = 0;
    }

    return true;
}

bool exports_sqlink_wasm_extension_remove_authorizer(
    exports_sqlink_wasm_extension_hook_handle_t handle,
    exports_sqlink_wasm_extension_extension_error_t *err
) {
    if (handle >= (uint64_t)g_hook_count) {
        set_extension_error(err, SQLITE_ERROR, "Invalid hook handle");
        return false;
    }

    HookRegistration *reg = &g_hooks[handle];
    if (reg->db) {
        sqlite3_set_authorizer(reg->db, NULL, NULL);
        reg->db = NULL;
    }

    return true;
}
