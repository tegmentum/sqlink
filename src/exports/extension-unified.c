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

/* The spi surface grew (limit / db-config-bool / deserialize-db /
 * execute-multi / open-db) when sqlite:extension reached @1.0.0. The
 * unified C glue is still the WIP "inspect the binding shape" scaffold
 * (see Makefile `unified` target), so these route through the same
 * not-yet-implemented stubs as the rest of the spi surface above. */
int32_t exports_sqlite_extension_spi_limit(int32_t category, int32_t value) {
    (void)category;
    (void)value;
    /* Returning the requested value is the no-op contract for an
     * unconfigured limit getter/setter. */
    return value;
}

bool exports_sqlite_extension_spi_db_config_bool(
    int32_t op,
    bool set,
    bool value,
    bool *ret,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)op;
    (void)set;
    (void)value;
    if (ret) {
        *ret = false;
    }
    fill_error(err, SQLITE_ERROR, "spi.db-config-bool not yet implemented");
    return false;
}

bool exports_sqlite_extension_spi_deserialize_db(
    sqlite_cli_unified_string_t *db_name,
    sqlite_cli_unified_list_u8_t *bytes,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)db_name;
    (void)bytes;
    fill_error(err, SQLITE_ERROR, "spi.deserialize-db not yet implemented");
    return false;
}

bool exports_sqlite_extension_spi_execute_multi(
    sqlite_cli_unified_string_t *sql,
    exports_sqlite_extension_spi_list_named_param_t *named_params,
    exports_sqlite_extension_spi_list_query_result_t *ret,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)sql;
    (void)named_params;
    (void)ret;
    fill_error(err, SQLITE_ERROR, "spi.execute-multi not yet implemented");
    return false;
}

bool exports_sqlite_extension_spi_open_db(
    sqlite_cli_unified_string_t *path,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)path;
    fill_error(err, SQLITE_ERROR, "spi.open-db not yet implemented");
    return false;
}

void exports_sqlite_extension_spi_list_vfs(
    sqlite_cli_unified_list_string_t *ret
) {
    /* No VFS enumeration on this scaffold yet: hand back an empty
     * list so the caller's owned-list free is well-defined. */
    if (ret) {
        ret->ptr = NULL;
        ret->len = 0;
    }
}

bool exports_sqlite_extension_spi_vfs_name(
    sqlite_cli_unified_string_t *db_name,
    sqlite_cli_unified_string_t *ret,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)db_name;
    if (ret) {
        ret->ptr = NULL;
        ret->len = 0;
    }
    fill_error(err, SQLITE_ERROR, "spi.vfs-name not yet implemented");
    return false;
}

bool exports_sqlite_extension_spi_serialize_db(
    sqlite_cli_unified_string_t *db_name,
    sqlite_cli_unified_list_u8_t *ret,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)db_name;
    if (ret) {
        ret->ptr = NULL;
        ret->len = 0;
    }
    fill_error(err, SQLITE_ERROR, "spi.serialize-db not yet implemented");
    return false;
}

bool exports_sqlite_extension_spi_backup_into(
    sqlite_cli_unified_string_t *src_db,
    sqlite_cli_unified_string_t *dst_path,
    sqlite_cli_unified_string_t *dst_db,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)src_db;
    (void)dst_path;
    (void)dst_db;
    fill_error(err, SQLITE_ERROR, "spi.backup-into not yet implemented");
    return false;
}

bool exports_sqlite_extension_spi_restore_from(
    sqlite_cli_unified_string_t *src_path,
    sqlite_cli_unified_string_t *src_db,
    sqlite_cli_unified_string_t *dst_db,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)src_path;
    (void)src_db;
    (void)dst_db;
    fill_error(err, SQLITE_ERROR, "spi.restore-from not yet implemented");
    return false;
}

bool exports_sqlite_extension_spi_set_busy_timeout(
    int32_t ms,
    exports_sqlite_extension_spi_sqlite_error_t *err
) {
    (void)ms;
    fill_error(err, SQLITE_ERROR, "spi.set-busy-timeout not yet implemented");
    return false;
}

int64_t exports_sqlite_extension_spi_changes(void) {
    return 0;
}

int64_t exports_sqlite_extension_spi_total_changes(void) {
    return 0;
}

int64_t exports_sqlite_extension_spi_last_insert_rowid(void) {
    return 0;
}

int64_t exports_sqlite_extension_spi_current_memory_used(void) {
    return 0;
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

/* ------------------------------------------------------------------ */
/* Slot registration + scalar-function dispatch                       */
/* ------------------------------------------------------------------ */

/* Identifies a single registered function in the dispatch table.
 * Stashed as sqlite3_create_function_v2's user-data argument. */
enum slot_id { SLOT_FTS5, SLOT_JSON1, SLOT_RTREE, SLOT_GEOPOLY, SLOT_DEMO, SLOT_COUNT };

typedef struct {
    enum slot_id slot;
    uint64_t func_id;
} dispatch_entry_t;

/* All dispatch entries live in a single arena so cleanup is trivial.
 * Capacity is sized large enough to hold every function across all
 * four extensions; if a manifest declares more than this, the
 * surplus is silently dropped (registration prints to stderr). */
#define MAX_DISPATCH_ENTRIES 256
static dispatch_entry_t g_dispatch[MAX_DISPATCH_ENTRIES];
static int g_dispatch_count = 0;

/* Forward decls for the per-slot describe() that the canonical
 * bindings expose. Each is a different generated typedef around the
 * same underlying layout, so this header copy is wit-bindgen-stable. */
extern void sqlink_wasm_fts5_slot_describe(sqlink_wasm_fts5_slot_manifest_t *ret);
extern bool sqlink_wasm_fts5_slot_call(uint64_t func_id,
                                       sqlink_wasm_fts5_slot_list_sql_value_t *args,
                                       sqlink_wasm_fts5_slot_sql_value_t *ret,
                                       sqlite_cli_unified_string_t *err);
extern void sqlink_wasm_json1_slot_describe(sqlink_wasm_json1_slot_manifest_t *ret);
extern bool sqlink_wasm_json1_slot_call(uint64_t func_id,
                                        sqlink_wasm_json1_slot_list_sql_value_t *args,
                                        sqlink_wasm_json1_slot_sql_value_t *ret,
                                        sqlite_cli_unified_string_t *err);
extern void sqlink_wasm_rtree_slot_describe(sqlink_wasm_rtree_slot_manifest_t *ret);
extern bool sqlink_wasm_rtree_slot_call(uint64_t func_id,
                                        sqlink_wasm_rtree_slot_list_sql_value_t *args,
                                        sqlink_wasm_rtree_slot_sql_value_t *ret,
                                        sqlite_cli_unified_string_t *err);
extern void sqlink_wasm_geopoly_slot_describe(sqlink_wasm_geopoly_slot_manifest_t *ret);
extern bool sqlink_wasm_geopoly_slot_call(uint64_t func_id,
                                          sqlink_wasm_geopoly_slot_list_sql_value_t *args,
                                          sqlink_wasm_geopoly_slot_sql_value_t *ret,
                                          sqlite_cli_unified_string_t *err);
extern void sqlink_wasm_demo_slot_describe(sqlink_wasm_demo_slot_manifest_t *ret);
extern bool sqlink_wasm_demo_slot_call(uint64_t func_id,
                                       sqlink_wasm_demo_slot_list_sql_value_t *args,
                                       sqlink_wasm_demo_slot_sql_value_t *ret,
                                       sqlite_cli_unified_string_t *err);

/* Convert one sqlite3_value to the canonical variant sql-value.
 * Memory: TEXT and BLOB cases copy bytes via malloc; the freed-by-cabi
 * side calls _free which releases them. */
static void sqlite_to_wit(sqlite3_value *v, sqlite_extension_types_sql_value_t *out) {
    switch (sqlite3_value_type(v)) {
        case SQLITE_INTEGER:
            out->tag = SQLITE_EXTENSION_TYPES_SQL_VALUE_INTEGER;
            out->val.integer = sqlite3_value_int64(v);
            break;
        case SQLITE_FLOAT:
            out->tag = SQLITE_EXTENSION_TYPES_SQL_VALUE_REAL;
            out->val.real = sqlite3_value_double(v);
            break;
        case SQLITE_TEXT: {
            const unsigned char *t = sqlite3_value_text(v);
            int n = sqlite3_value_bytes(v);
            out->tag = SQLITE_EXTENSION_TYPES_SQL_VALUE_TEXT;
            out->val.text.ptr = (uint8_t *)malloc(n);
            if (out->val.text.ptr) {
                memcpy(out->val.text.ptr, t, n);
                out->val.text.len = n;
            } else {
                out->val.text.len = 0;
            }
            break;
        }
        case SQLITE_BLOB: {
            const void *b = sqlite3_value_blob(v);
            int n = sqlite3_value_bytes(v);
            out->tag = SQLITE_EXTENSION_TYPES_SQL_VALUE_BLOB;
            out->val.blob.ptr = (uint8_t *)malloc(n);
            if (out->val.blob.ptr) {
                memcpy(out->val.blob.ptr, b, n);
                out->val.blob.len = n;
            } else {
                out->val.blob.len = 0;
            }
            break;
        }
        case SQLITE_NULL:
        default:
            out->tag = SQLITE_EXTENSION_TYPES_SQL_VALUE_NULL;
            break;
    }
}

/* Set sqlite3_result_* from a canonical variant sql-value.
 * Memory: TEXT and BLOB use SQLITE_TRANSIENT so SQLite copies. */
static void wit_to_sqlite_result(sqlite3_context *ctx,
                                 sqlite_extension_types_sql_value_t *v) {
    switch (v->tag) {
        case SQLITE_EXTENSION_TYPES_SQL_VALUE_INTEGER:
            sqlite3_result_int64(ctx, v->val.integer);
            break;
        case SQLITE_EXTENSION_TYPES_SQL_VALUE_REAL:
            sqlite3_result_double(ctx, v->val.real);
            break;
        case SQLITE_EXTENSION_TYPES_SQL_VALUE_TEXT:
            sqlite3_result_text(ctx,
                                (const char *)v->val.text.ptr,
                                v->val.text.len, SQLITE_TRANSIENT);
            break;
        case SQLITE_EXTENSION_TYPES_SQL_VALUE_BLOB:
            sqlite3_result_blob(ctx,
                                v->val.blob.ptr, v->val.blob.len, SQLITE_TRANSIENT);
            break;
        case SQLITE_EXTENSION_TYPES_SQL_VALUE_NULL:
        default:
            sqlite3_result_null(ctx);
            break;
    }
}

/* SQLite scalar-function trampoline. Routes the call to the matching
 * slot based on the dispatch entry stashed as user-data.
 *
 * All four slots share the same underlying sql-value struct (via
 * typedef chains in the generated bindings), so the args buffer can
 * be reused; the call() signature is per-slot only because the type
 * names differ. */
static void wasm_ext_xfunc(sqlite3_context *ctx, int argc, sqlite3_value **argv) {
    dispatch_entry_t *d = (dispatch_entry_t *)sqlite3_user_data(ctx);

    /* Build the wit args list using the canonical underlying type. */
    sqlite_extension_types_sql_value_t *args =
        (sqlite_extension_types_sql_value_t *)malloc(
            sizeof(*args) * (argc > 0 ? argc : 1));
    if (!args) {
        sqlite3_result_error_nomem(ctx);
        return;
    }
    for (int i = 0; i < argc; i++) {
        sqlite_to_wit(argv[i], &args[i]);
    }

    sqlite_extension_types_sql_value_t ret;
    sqlite_cli_unified_string_t err = {NULL, 0};
    bool ok;

    /* Each slot's call() has its own typedef'd args/ret types that
     * boil down to the canonical layout. The casts are safe because
     * the generated typedefs are aliases (no struct duplication). */
    switch (d->slot) {
        case SLOT_FTS5: {
            sqlink_wasm_fts5_slot_list_sql_value_t slot_args = {
                (sqlink_wasm_fts5_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlink_wasm_fts5_slot_call(d->func_id, &slot_args,
                                            (sqlink_wasm_fts5_slot_sql_value_t *)&ret, &err);
            break;
        }
        case SLOT_JSON1: {
            sqlink_wasm_json1_slot_list_sql_value_t slot_args = {
                (sqlink_wasm_json1_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlink_wasm_json1_slot_call(d->func_id, &slot_args,
                                             (sqlink_wasm_json1_slot_sql_value_t *)&ret, &err);
            break;
        }
        case SLOT_RTREE: {
            sqlink_wasm_rtree_slot_list_sql_value_t slot_args = {
                (sqlink_wasm_rtree_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlink_wasm_rtree_slot_call(d->func_id, &slot_args,
                                             (sqlink_wasm_rtree_slot_sql_value_t *)&ret, &err);
            break;
        }
        case SLOT_GEOPOLY: {
            sqlink_wasm_geopoly_slot_list_sql_value_t slot_args = {
                (sqlink_wasm_geopoly_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlink_wasm_geopoly_slot_call(d->func_id, &slot_args,
                                               (sqlink_wasm_geopoly_slot_sql_value_t *)&ret, &err);
            break;
        }
        case SLOT_DEMO: {
            sqlink_wasm_demo_slot_list_sql_value_t slot_args = {
                (sqlink_wasm_demo_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlink_wasm_demo_slot_call(d->func_id, &slot_args,
                                            (sqlink_wasm_demo_slot_sql_value_t *)&ret, &err);
            break;
        }
        default:
            free(args);
            sqlite3_result_error(ctx, "unknown slot", -1);
            return;
    }

    free(args);

    if (!ok) {
        char *emsg = to_cstring(&err);
        sqlite3_result_error(ctx, emsg ? emsg : "extension error", -1);
        free(emsg);
        sqlite_cli_unified_string_free(&err);
        return;
    }

    wit_to_sqlite_result(ctx, &ret);
    sqlite_extension_types_sql_value_free(&ret);
}

/* Register every scalar-function in `manifest` with `db`, attaching a
 * dispatch entry for `slot`. The entries live in the static arena and
 * never need explicit cleanup. */
static void register_slot_scalars(sqlite3 *db, enum slot_id slot,
                                  const sqlink_wasm_fts5_slot_manifest_t *manifest) {
    /* All four manifest types are typedef aliases of the same
     * underlying record so a single signature handles all of them.
     * The scalar-function spec type is package-level (not per-slot
     * typedef), so we reference it directly. */
    for (size_t i = 0; i < manifest->scalar_functions.len; i++) {
        sqlite_extension_metadata_scalar_function_spec_t *spec =
            &manifest->scalar_functions.ptr[i];

        if (g_dispatch_count >= MAX_DISPATCH_ENTRIES) {
            fprintf(stderr,
                    "extension-unified: dispatch table full; dropping function\n");
            break;
        }

        dispatch_entry_t *entry = &g_dispatch[g_dispatch_count++];
        entry->slot = slot;
        entry->func_id = spec->id;

        /* Manifest name is wit-bindgen-style (ptr+len, not null-terminated). */
        char *name = malloc(spec->name.len + 1);
        if (!name) continue;
        memcpy(name, spec->name.ptr, spec->name.len);
        name[spec->name.len] = '\0';

        int flags = SQLITE_UTF8;
        if (spec->func_flags & 1 /* deterministic */) {
            flags |= SQLITE_DETERMINISTIC;
        }
        if (spec->func_flags & 2 /* direct-only */) {
            flags |= SQLITE_DIRECTONLY;
        }
        if (spec->func_flags & 4 /* innocuous */) {
            flags |= SQLITE_INNOCUOUS;
        }

        sqlite3_create_function_v2(db, name, spec->num_args, flags,
                                   entry, wasm_ext_xfunc, NULL, NULL, NULL);
        free(name);
    }
}

/* Iterate all four slots' describe() outputs and register their
 * scalar functions on the given connection. Called from
 * sqlite3_auto_extension for every newly opened database. */
static int register_all_slots(sqlite3 *db, char **pzErrMsg,
                              const struct sqlite3_api_routines *pThunk) {
    (void)pzErrMsg;
    (void)pThunk;

    /* The four manifest types are typedef aliases of one underlying
     * struct; we treat them via the fts5 variant for the shared
     * register_slot_scalars routine. */
    {
        sqlink_wasm_fts5_slot_manifest_t m;
        sqlink_wasm_fts5_slot_describe(&m);
        register_slot_scalars(db, SLOT_FTS5, &m);
        sqlink_wasm_fts5_slot_manifest_free(&m);
    }
    {
        sqlink_wasm_json1_slot_manifest_t m;
        sqlink_wasm_json1_slot_describe(&m);
        register_slot_scalars(db, SLOT_JSON1, (sqlink_wasm_fts5_slot_manifest_t *)&m);
        sqlink_wasm_json1_slot_manifest_free(&m);
    }
    {
        sqlink_wasm_rtree_slot_manifest_t m;
        sqlink_wasm_rtree_slot_describe(&m);
        register_slot_scalars(db, SLOT_RTREE, (sqlink_wasm_fts5_slot_manifest_t *)&m);
        sqlink_wasm_rtree_slot_manifest_free(&m);
    }
    {
        sqlink_wasm_geopoly_slot_manifest_t m;
        sqlink_wasm_geopoly_slot_describe(&m);
        register_slot_scalars(db, SLOT_GEOPOLY, (sqlink_wasm_fts5_slot_manifest_t *)&m);
        sqlink_wasm_geopoly_slot_manifest_free(&m);
    }
    {
        sqlink_wasm_demo_slot_manifest_t m;
        sqlink_wasm_demo_slot_describe(&m);
        register_slot_scalars(db, SLOT_DEMO, (sqlink_wasm_fts5_slot_manifest_t *)&m);
        sqlink_wasm_demo_slot_manifest_free(&m);
    }
    return SQLITE_OK;
}

/* ------------------------------------------------------------------ */
/* Dynamic dispatch — for extensions loaded via .load at runtime      */
/* ------------------------------------------------------------------ */

typedef struct {
    char *ext_name;
    uint64_t func_id;
} dynamic_dispatch_t;

static void wasm_dyn_xfunc(sqlite3_context *ctx, int argc, sqlite3_value **argv) {
    dynamic_dispatch_t *d = (dynamic_dispatch_t *)sqlite3_user_data(ctx);

    sqlite_extension_types_sql_value_t *args =
        (sqlite_extension_types_sql_value_t *)malloc(
            sizeof(*args) * (argc > 0 ? argc : 1));
    if (!args) {
        sqlite3_result_error_nomem(ctx);
        return;
    }
    for (int i = 0; i < argc; i++) {
        sqlite_to_wit(argv[i], &args[i]);
    }

    sqlite_cli_unified_string_t ext_name_wit = {
        (uint8_t *)d->ext_name, strlen(d->ext_name)};
    sqlink_wasm_dispatch_list_sql_value_t slot_args = {
        (sqlink_wasm_dispatch_sql_value_t *)args, (size_t)argc};
    sqlite_extension_types_sql_value_t ret;
    sqlite_cli_unified_string_t err = {NULL, 0};

    bool ok = sqlink_wasm_dispatch_scalar_call(
        &ext_name_wit, d->func_id, &slot_args,
        (sqlink_wasm_dispatch_sql_value_t *)&ret, &err);
    free(args);

    if (!ok) {
        char *emsg = to_cstring(&err);
        sqlite3_result_error(ctx, emsg ? emsg : "dispatch error", -1);
        free(emsg);
        sqlite_cli_unified_string_free(&err);
        return;
    }
    wit_to_sqlite_result(ctx, &ret);
    sqlite_extension_types_sql_value_free(&ret);
}

/* Per-aggregation context-id counter. SQLite calls xStep with the
 * same `sqlite3_context*` for every row in one aggregation; the
 * aggregate-context memory it allocates (via
 * `sqlite3_aggregate_context`) is where we stash the id we give the
 * loaded extension so it can thread its running state between step
 * and finalize. Counter is monotonic per process. */
static uint64_t g_agg_ctx_counter = 1;

/* Look up (or allocate) the host-side context id for this
 * aggregation. First call for a given xStep series allocates fresh
 * memory and writes a new counter value; subsequent calls return
 * the stored id. Returns 0 on allocation failure (loaded extension
 * should treat that as "no state available"). */
static uint64_t agg_ctx_id(sqlite3_context *ctx) {
    uint64_t *slot = (uint64_t *)sqlite3_aggregate_context(ctx, sizeof(uint64_t));
    if (!slot) return 0;
    if (*slot == 0) {
        *slot = g_agg_ctx_counter++;
        /* g_agg_ctx_counter wraps after 2^64 calls — fine. */
    }
    return *slot;
}

static void wasm_dyn_xstep(sqlite3_context *ctx, int argc, sqlite3_value **argv) {
    dynamic_dispatch_t *d = (dynamic_dispatch_t *)sqlite3_user_data(ctx);

    sqlite_extension_types_sql_value_t *args =
        (sqlite_extension_types_sql_value_t *)malloc(
            sizeof(*args) * (argc > 0 ? argc : 1));
    if (!args) {
        sqlite3_result_error_nomem(ctx);
        return;
    }
    for (int i = 0; i < argc; i++) {
        sqlite_to_wit(argv[i], &args[i]);
    }

    sqlite_cli_unified_string_t ext_name_wit = {
        (uint8_t *)d->ext_name, strlen(d->ext_name)};
    sqlink_wasm_dispatch_list_sql_value_t slot_args = {
        (sqlink_wasm_dispatch_sql_value_t *)args, (size_t)argc};
    sqlite_cli_unified_string_t err = {NULL, 0};
    uint64_t cid = agg_ctx_id(ctx);

    bool ok = sqlink_wasm_dispatch_aggregate_step(
        &ext_name_wit, d->func_id, cid, &slot_args, &err);
    free(args);

    if (!ok) {
        char *emsg = to_cstring(&err);
        sqlite3_result_error(ctx, emsg ? emsg : "aggregate-step error", -1);
        free(emsg);
        sqlite_cli_unified_string_free(&err);
    }
}

static void wasm_dyn_xfinal(sqlite3_context *ctx) {
    dynamic_dispatch_t *d = (dynamic_dispatch_t *)sqlite3_user_data(ctx);

    sqlite_cli_unified_string_t ext_name_wit = {
        (uint8_t *)d->ext_name, strlen(d->ext_name)};
    sqlite_extension_types_sql_value_t ret;
    sqlite_cli_unified_string_t err = {NULL, 0};
    /* xFinal is called once per aggregation regardless of whether
     * xStep ran (e.g. SELECT agg() with no rows). sqlite3_aggregate_
     * context with size=0 returns existing memory if any, else NULL.
     * If NULL, hand the extension a 0 id — it can treat that as
     * "no rows seen, return your zero value". */
    uint64_t *slot = (uint64_t *)sqlite3_aggregate_context(ctx, 0);
    uint64_t cid = slot ? *slot : 0;

    bool ok = sqlink_wasm_dispatch_aggregate_finalize(
        &ext_name_wit, d->func_id, cid, &ret, &err);
    if (!ok) {
        char *emsg = to_cstring(&err);
        sqlite3_result_error(ctx, emsg ? emsg : "aggregate-finalize error", -1);
        free(emsg);
        sqlite_cli_unified_string_free(&err);
        return;
    }
    wit_to_sqlite_result(ctx, &ret);
    sqlite_extension_types_sql_value_free(&ret);
}

/* Dispatch entry for a registered collation. Same shape as
 * dynamic_dispatch_t but referenced via sqlite3_create_collation's
 * pArg instead of sqlite3_user_data. */
typedef struct {
    char *ext_name;
    uint64_t collation_id;
} dynamic_collation_t;

static int wasm_dyn_xcompare(
    void *pArg,
    int n1, const void *p1,
    int n2, const void *p2
) {
    dynamic_collation_t *c = (dynamic_collation_t *)pArg;

    sqlite_cli_unified_string_t ext_name_wit = {
        (uint8_t *)c->ext_name, strlen(c->ext_name)};
    sqlite_cli_unified_string_t a = {(uint8_t *)p1, (size_t)n1};
    sqlite_cli_unified_string_t b = {(uint8_t *)p2, (size_t)n2};

    int32_t r = sqlink_wasm_dispatch_collation_compare(
        &ext_name_wit, c->collation_id, &a, &b);
    return (int)r;
}

/* Dispatch entry for an installed hook (authorizer / update / commit
 * / rollback). Only the ext-name is needed since hook callbacks
 * don't carry a per-callback id — there's at most one hook of each
 * kind per db (sqlite3_set_authorizer etc. are db-global). */
typedef struct {
    char *ext_name;
} dynamic_hook_t;

/* Map a SQLite SQLITE_* action code to our generated auth-action
 * enum. The enum's declaration order in types.wit matches the order
 * below; if we add new variants there we have to extend this. */
static sqlink_wasm_dispatch_auth_action_t sqlite_action_to_wit(int op) {
    switch (op) {
        case SQLITE_CREATE_INDEX:        return SQLITE_EXTENSION_TYPES_AUTH_ACTION_CREATE_INDEX;
        case SQLITE_CREATE_TABLE:        return SQLITE_EXTENSION_TYPES_AUTH_ACTION_CREATE_TABLE;
        case SQLITE_CREATE_TEMP_INDEX:   return SQLITE_EXTENSION_TYPES_AUTH_ACTION_CREATE_TEMP_INDEX;
        case SQLITE_CREATE_TEMP_TABLE:   return SQLITE_EXTENSION_TYPES_AUTH_ACTION_CREATE_TEMP_TABLE;
        case SQLITE_CREATE_TEMP_TRIGGER: return SQLITE_EXTENSION_TYPES_AUTH_ACTION_CREATE_TEMP_TRIGGER;
        case SQLITE_CREATE_TEMP_VIEW:    return SQLITE_EXTENSION_TYPES_AUTH_ACTION_CREATE_TEMP_VIEW;
        case SQLITE_CREATE_TRIGGER:      return SQLITE_EXTENSION_TYPES_AUTH_ACTION_CREATE_TRIGGER;
        case SQLITE_CREATE_VIEW:         return SQLITE_EXTENSION_TYPES_AUTH_ACTION_CREATE_VIEW;
        case SQLITE_DELETE:              return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DELETE;
        case SQLITE_DROP_INDEX:          return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DROP_INDEX;
        case SQLITE_DROP_TABLE:          return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DROP_TABLE;
        case SQLITE_DROP_TEMP_INDEX:     return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DROP_TEMP_INDEX;
        case SQLITE_DROP_TEMP_TABLE:     return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DROP_TEMP_TABLE;
        case SQLITE_DROP_TEMP_TRIGGER:   return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DROP_TEMP_TRIGGER;
        case SQLITE_DROP_TEMP_VIEW:      return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DROP_TEMP_VIEW;
        case SQLITE_DROP_TRIGGER:        return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DROP_TRIGGER;
        case SQLITE_DROP_VIEW:           return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DROP_VIEW;
        case SQLITE_INSERT:              return SQLITE_EXTENSION_TYPES_AUTH_ACTION_INSERT;
        case SQLITE_PRAGMA:              return SQLITE_EXTENSION_TYPES_AUTH_ACTION_PRAGMA;
        case SQLITE_READ:                return SQLITE_EXTENSION_TYPES_AUTH_ACTION_READ;
        case SQLITE_SELECT:              return SQLITE_EXTENSION_TYPES_AUTH_ACTION_SELECT;
        case SQLITE_TRANSACTION:         return SQLITE_EXTENSION_TYPES_AUTH_ACTION_TRANSACTION;
        case SQLITE_UPDATE:              return SQLITE_EXTENSION_TYPES_AUTH_ACTION_UPDATE;
        case SQLITE_ATTACH:              return SQLITE_EXTENSION_TYPES_AUTH_ACTION_ATTACH;
        case SQLITE_DETACH:              return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DETACH;
        case SQLITE_ALTER_TABLE:         return SQLITE_EXTENSION_TYPES_AUTH_ACTION_ALTER_TABLE;
        case SQLITE_REINDEX:             return SQLITE_EXTENSION_TYPES_AUTH_ACTION_REINDEX;
        case SQLITE_ANALYZE:             return SQLITE_EXTENSION_TYPES_AUTH_ACTION_ANALYZE;
        case SQLITE_CREATE_VTABLE:       return SQLITE_EXTENSION_TYPES_AUTH_ACTION_CREATE_VTABLE;
        case SQLITE_DROP_VTABLE:         return SQLITE_EXTENSION_TYPES_AUTH_ACTION_DROP_VTABLE;
        case SQLITE_FUNCTION:            return SQLITE_EXTENSION_TYPES_AUTH_ACTION_FUNCTION;
        case SQLITE_SAVEPOINT:           return SQLITE_EXTENSION_TYPES_AUTH_ACTION_SAVEPOINT;
        case SQLITE_RECURSIVE:           return SQLITE_EXTENSION_TYPES_AUTH_ACTION_RECURSIVE;
        /* Newer codes (COPY etc.) aren't represented in our WIT enum
         * yet; route them through `read` as a safe default. */
        default:                         return SQLITE_EXTENSION_TYPES_AUTH_ACTION_READ;
    }
}

static int wit_auth_result_to_sqlite(sqlink_wasm_dispatch_auth_result_t r) {
    switch (r) {
        case SQLITE_EXTENSION_TYPES_AUTH_RESULT_OK:     return SQLITE_OK;
        case SQLITE_EXTENSION_TYPES_AUTH_RESULT_DENY:   return SQLITE_DENY;
        case SQLITE_EXTENSION_TYPES_AUTH_RESULT_IGNORE: return SQLITE_IGNORE;
        default:                                      return SQLITE_OK;
    }
}

static int wasm_dyn_xauthorizer(
    void *pArg, int op,
    const char *a1, const char *a2,
    const char *db, const char *trigger
) {
    dynamic_hook_t *h = (dynamic_hook_t *)pArg;
    sqlite_cli_unified_string_t ext_name_wit = {
        (uint8_t *)h->ext_name, strlen(h->ext_name)};

    /* The four optionals: SQLite passes NULL for absent arguments. */
    sqlite_cli_unified_string_t a1w, a2w, dbw, trw;
    sqlite_cli_unified_string_t *pa1 = NULL, *pa2 = NULL,
                                *pdb = NULL, *ptr = NULL;
    if (a1)      { a1w = (sqlite_cli_unified_string_t){(uint8_t *)a1, strlen(a1)}; pa1 = &a1w; }
    if (a2)      { a2w = (sqlite_cli_unified_string_t){(uint8_t *)a2, strlen(a2)}; pa2 = &a2w; }
    if (db)      { dbw = (sqlite_cli_unified_string_t){(uint8_t *)db, strlen(db)}; pdb = &dbw; }
    if (trigger) { trw = (sqlite_cli_unified_string_t){(uint8_t *)trigger, strlen(trigger)}; ptr = &trw; }

    sqlink_wasm_dispatch_auth_result_t r = sqlink_wasm_dispatch_authorize(
        &ext_name_wit, sqlite_action_to_wit(op), pa1, pa2, pdb, ptr);
    return wit_auth_result_to_sqlite(r);
}

static void wasm_dyn_xupdate(
    void *pArg, int op,
    char const *db, char const *table,
    sqlite3_int64 rowid
) {
    dynamic_hook_t *h = (dynamic_hook_t *)pArg;
    sqlite_cli_unified_string_t ext_name_wit = {
        (uint8_t *)h->ext_name, strlen(h->ext_name)};

    sqlink_wasm_dispatch_update_operation_t wit_op;
    switch (op) {
        case SQLITE_INSERT: wit_op = SQLITE_EXTENSION_TYPES_UPDATE_OPERATION_INSERT; break;
        case SQLITE_UPDATE: wit_op = SQLITE_EXTENSION_TYPES_UPDATE_OPERATION_UPDATE; break;
        case SQLITE_DELETE: wit_op = SQLITE_EXTENSION_TYPES_UPDATE_OPERATION_DELETE; break;
        default: return;
    }

    sqlite_cli_unified_string_t dbw = {
        (uint8_t *)(db ? db : ""), db ? strlen(db) : 0};
    sqlite_cli_unified_string_t tw = {
        (uint8_t *)(table ? table : ""), table ? strlen(table) : 0};
    sqlink_wasm_dispatch_on_update(&ext_name_wit, wit_op, &dbw, &tw, rowid);
}

/* SQLite's commit hook returns non-zero to convert the commit to a
 * rollback. Our WIT returns bool where true = proceed. Invert. */
static int wasm_dyn_xcommit(void *pArg) {
    dynamic_hook_t *h = (dynamic_hook_t *)pArg;
    sqlite_cli_unified_string_t ext_name_wit = {
        (uint8_t *)h->ext_name, strlen(h->ext_name)};
    return sqlink_wasm_dispatch_on_commit(&ext_name_wit) ? 0 : 1;
}

static void wasm_dyn_xrollback(void *pArg) {
    dynamic_hook_t *h = (dynamic_hook_t *)pArg;
    sqlite_cli_unified_string_t ext_name_wit = {
        (uint8_t *)h->ext_name, strlen(h->ext_name)};
    sqlink_wasm_dispatch_on_rollback(&ext_name_wit);
}

void wasm_register_dynamic_manifest(
    sqlite3 *db,
    const char *ext_name,
    const sqlite_extension_metadata_manifest_t *manifest
) {
    /* Scalar functions */
    for (size_t i = 0; i < manifest->scalar_functions.len; i++) {
        sqlite_extension_metadata_scalar_function_spec_t *spec =
            &manifest->scalar_functions.ptr[i];

        dynamic_dispatch_t *entry =
            (dynamic_dispatch_t *)malloc(sizeof(*entry));
        if (!entry) continue;
        entry->ext_name = strdup(ext_name);
        if (!entry->ext_name) { free(entry); continue; }
        entry->func_id = spec->id;

        char *name = malloc(spec->name.len + 1);
        if (!name) { free(entry->ext_name); free(entry); continue; }
        memcpy(name, spec->name.ptr, spec->name.len);
        name[spec->name.len] = '\0';

        int flags = SQLITE_UTF8;
        if (spec->func_flags & 1) flags |= SQLITE_DETERMINISTIC;
        if (spec->func_flags & 2) flags |= SQLITE_DIRECTONLY;
        if (spec->func_flags & 4) flags |= SQLITE_INNOCUOUS;

        sqlite3_create_function_v2(db, name, spec->num_args, flags,
                                   entry, wasm_dyn_xfunc, NULL, NULL, NULL);
        free(name);
    }

    /* Aggregate functions (step + finalize). The window-mode
     * methods (value + inverse) aren't dispatched yet. */
    for (size_t i = 0; i < manifest->aggregate_functions.len; i++) {
        sqlite_extension_metadata_aggregate_function_spec_t *spec =
            &manifest->aggregate_functions.ptr[i];

        dynamic_dispatch_t *entry =
            (dynamic_dispatch_t *)malloc(sizeof(*entry));
        if (!entry) continue;
        entry->ext_name = strdup(ext_name);
        if (!entry->ext_name) { free(entry); continue; }
        entry->func_id = spec->id;

        char *name = malloc(spec->name.len + 1);
        if (!name) { free(entry->ext_name); free(entry); continue; }
        memcpy(name, spec->name.ptr, spec->name.len);
        name[spec->name.len] = '\0';

        int flags = SQLITE_UTF8;
        if (spec->func_flags & 1) flags |= SQLITE_DETERMINISTIC;
        if (spec->func_flags & 2) flags |= SQLITE_DIRECTONLY;
        if (spec->func_flags & 4) flags |= SQLITE_INNOCUOUS;

        sqlite3_create_function_v2(
            db, name, spec->num_args, flags, entry,
            NULL,                   /* xFunc — N/A for aggregates */
            wasm_dyn_xstep,
            wasm_dyn_xfinal,
            NULL                    /* xDestroy */
        );
        free(name);
    }

    /* Custom collations */
    for (size_t i = 0; i < manifest->collations.len; i++) {
        sqlite_extension_metadata_collation_spec_t *spec =
            &manifest->collations.ptr[i];

        dynamic_collation_t *entry =
            (dynamic_collation_t *)malloc(sizeof(*entry));
        if (!entry) continue;
        entry->ext_name = strdup(ext_name);
        if (!entry->ext_name) { free(entry); continue; }
        entry->collation_id = spec->id;

        char *name = malloc(spec->name.len + 1);
        if (!name) { free(entry->ext_name); free(entry); continue; }
        memcpy(name, spec->name.ptr, spec->name.len);
        name[spec->name.len] = '\0';

        sqlite3_create_collation_v2(
            db, name, SQLITE_UTF8,
            entry, wasm_dyn_xcompare,
            NULL                    /* xDestroy */
        );
        free(name);
    }

    /* Hooks. SQLite's sqlite3_set_authorizer / update_hook /
     * commit_hook / rollback_hook are db-global: at most one
     * callback can be active per db. Loading a second hook-class
     * extension overwrites the first. We log via stderr (the host's
     * own logger isn't reachable from this side) so the operator
     * sees the displaced extension. */
    if (manifest->has_authorizer) {
        dynamic_hook_t *h = (dynamic_hook_t *)malloc(sizeof(*h));
        if (h) {
            h->ext_name = strdup(ext_name);
            if (h->ext_name) {
                sqlite3_set_authorizer(db, wasm_dyn_xauthorizer, h);
            } else {
                free(h);
            }
        }
    }
    if (manifest->has_update_hook) {
        dynamic_hook_t *h = (dynamic_hook_t *)malloc(sizeof(*h));
        if (h) {
            h->ext_name = strdup(ext_name);
            if (h->ext_name) {
                sqlite3_update_hook(db, wasm_dyn_xupdate, h);
            } else {
                free(h);
            }
        }
    }
    if (manifest->has_commit_hook) {
        dynamic_hook_t *commit_h = (dynamic_hook_t *)malloc(sizeof(*commit_h));
        dynamic_hook_t *rb_h = (dynamic_hook_t *)malloc(sizeof(*rb_h));
        if (commit_h && rb_h) {
            commit_h->ext_name = strdup(ext_name);
            rb_h->ext_name = strdup(ext_name);
            if (commit_h->ext_name && rb_h->ext_name) {
                sqlite3_commit_hook(db, wasm_dyn_xcommit, commit_h);
                sqlite3_rollback_hook(db, wasm_dyn_xrollback, rb_h);
            } else {
                free(commit_h->ext_name);
                free(commit_h);
                free(rb_h->ext_name);
                free(rb_h);
            }
        } else {
            free(commit_h);
            free(rb_h);
        }
    }
}

/* Tear down anything wasm_register_dynamic_manifest installed.
 * Called from the `.unload` path so trampolines don't fire into a
 * dropped extension. SQLite's hook functions accept NULL to disable
 * them; create_function / create_collation entries can be removed
 * by re-registering with the same name and NULL callbacks. The
 * per-trampoline malloc'd dispatch entries leak — that's bounded
 * by the number of register/unregister cycles per process, which
 * is small. */
void wasm_unregister_dynamic_manifest(
    sqlite3 *db,
    const sqlite_extension_metadata_manifest_t *manifest
) {
    for (size_t i = 0; i < manifest->scalar_functions.len; i++) {
        sqlite_extension_metadata_scalar_function_spec_t *spec =
            &manifest->scalar_functions.ptr[i];
        char *name = malloc(spec->name.len + 1);
        if (!name) continue;
        memcpy(name, spec->name.ptr, spec->name.len);
        name[spec->name.len] = '\0';
        sqlite3_create_function_v2(db, name, spec->num_args, SQLITE_UTF8,
                                   NULL, NULL, NULL, NULL, NULL);
        free(name);
    }
    for (size_t i = 0; i < manifest->aggregate_functions.len; i++) {
        sqlite_extension_metadata_aggregate_function_spec_t *spec =
            &manifest->aggregate_functions.ptr[i];
        char *name = malloc(spec->name.len + 1);
        if (!name) continue;
        memcpy(name, spec->name.ptr, spec->name.len);
        name[spec->name.len] = '\0';
        sqlite3_create_function_v2(db, name, spec->num_args, SQLITE_UTF8,
                                   NULL, NULL, NULL, NULL, NULL);
        free(name);
    }
    for (size_t i = 0; i < manifest->collations.len; i++) {
        sqlite_extension_metadata_collation_spec_t *spec =
            &manifest->collations.ptr[i];
        char *name = malloc(spec->name.len + 1);
        if (!name) continue;
        memcpy(name, spec->name.ptr, spec->name.len);
        name[spec->name.len] = '\0';
        sqlite3_create_collation_v2(db, name, SQLITE_UTF8, NULL, NULL, NULL);
        free(name);
    }
    if (manifest->has_authorizer)  sqlite3_set_authorizer(db, NULL, NULL);
    if (manifest->has_update_hook) sqlite3_update_hook(db, NULL, NULL);
    if (manifest->has_commit_hook) {
        sqlite3_commit_hook(db, NULL, NULL);
        sqlite3_rollback_hook(db, NULL, NULL);
    }
}

/* Wire register_all_slots into SQLite's auto-extension chain at
 * component init time. Runs once per process; SQLite invokes the
 * registered fn for every sqlite3_open. */
__attribute__((constructor))
static void wire_slot_registration(void) {
    sqlite3_auto_extension((void (*)(void))register_all_slots);
}
