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
extern void sqlite_wasm_fts5_slot_describe(sqlite_wasm_fts5_slot_manifest_t *ret);
extern bool sqlite_wasm_fts5_slot_call(uint64_t func_id,
                                       sqlite_wasm_fts5_slot_list_sql_value_t *args,
                                       sqlite_wasm_fts5_slot_sql_value_t *ret,
                                       sqlite_cli_unified_string_t *err);
extern void sqlite_wasm_json1_slot_describe(sqlite_wasm_json1_slot_manifest_t *ret);
extern bool sqlite_wasm_json1_slot_call(uint64_t func_id,
                                        sqlite_wasm_json1_slot_list_sql_value_t *args,
                                        sqlite_wasm_json1_slot_sql_value_t *ret,
                                        sqlite_cli_unified_string_t *err);
extern void sqlite_wasm_rtree_slot_describe(sqlite_wasm_rtree_slot_manifest_t *ret);
extern bool sqlite_wasm_rtree_slot_call(uint64_t func_id,
                                        sqlite_wasm_rtree_slot_list_sql_value_t *args,
                                        sqlite_wasm_rtree_slot_sql_value_t *ret,
                                        sqlite_cli_unified_string_t *err);
extern void sqlite_wasm_geopoly_slot_describe(sqlite_wasm_geopoly_slot_manifest_t *ret);
extern bool sqlite_wasm_geopoly_slot_call(uint64_t func_id,
                                          sqlite_wasm_geopoly_slot_list_sql_value_t *args,
                                          sqlite_wasm_geopoly_slot_sql_value_t *ret,
                                          sqlite_cli_unified_string_t *err);
extern void sqlite_wasm_demo_slot_describe(sqlite_wasm_demo_slot_manifest_t *ret);
extern bool sqlite_wasm_demo_slot_call(uint64_t func_id,
                                       sqlite_wasm_demo_slot_list_sql_value_t *args,
                                       sqlite_wasm_demo_slot_sql_value_t *ret,
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
            sqlite_wasm_fts5_slot_list_sql_value_t slot_args = {
                (sqlite_wasm_fts5_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlite_wasm_fts5_slot_call(d->func_id, &slot_args,
                                            (sqlite_wasm_fts5_slot_sql_value_t *)&ret, &err);
            break;
        }
        case SLOT_JSON1: {
            sqlite_wasm_json1_slot_list_sql_value_t slot_args = {
                (sqlite_wasm_json1_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlite_wasm_json1_slot_call(d->func_id, &slot_args,
                                             (sqlite_wasm_json1_slot_sql_value_t *)&ret, &err);
            break;
        }
        case SLOT_RTREE: {
            sqlite_wasm_rtree_slot_list_sql_value_t slot_args = {
                (sqlite_wasm_rtree_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlite_wasm_rtree_slot_call(d->func_id, &slot_args,
                                             (sqlite_wasm_rtree_slot_sql_value_t *)&ret, &err);
            break;
        }
        case SLOT_GEOPOLY: {
            sqlite_wasm_geopoly_slot_list_sql_value_t slot_args = {
                (sqlite_wasm_geopoly_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlite_wasm_geopoly_slot_call(d->func_id, &slot_args,
                                               (sqlite_wasm_geopoly_slot_sql_value_t *)&ret, &err);
            break;
        }
        case SLOT_DEMO: {
            sqlite_wasm_demo_slot_list_sql_value_t slot_args = {
                (sqlite_wasm_demo_slot_sql_value_t *)args, (size_t)argc};
            ok = sqlite_wasm_demo_slot_call(d->func_id, &slot_args,
                                            (sqlite_wasm_demo_slot_sql_value_t *)&ret, &err);
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
                                  const sqlite_wasm_fts5_slot_manifest_t *manifest) {
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
        sqlite_wasm_fts5_slot_manifest_t m;
        sqlite_wasm_fts5_slot_describe(&m);
        register_slot_scalars(db, SLOT_FTS5, &m);
        sqlite_wasm_fts5_slot_manifest_free(&m);
    }
    {
        sqlite_wasm_json1_slot_manifest_t m;
        sqlite_wasm_json1_slot_describe(&m);
        register_slot_scalars(db, SLOT_JSON1, (sqlite_wasm_fts5_slot_manifest_t *)&m);
        sqlite_wasm_json1_slot_manifest_free(&m);
    }
    {
        sqlite_wasm_rtree_slot_manifest_t m;
        sqlite_wasm_rtree_slot_describe(&m);
        register_slot_scalars(db, SLOT_RTREE, (sqlite_wasm_fts5_slot_manifest_t *)&m);
        sqlite_wasm_rtree_slot_manifest_free(&m);
    }
    {
        sqlite_wasm_geopoly_slot_manifest_t m;
        sqlite_wasm_geopoly_slot_describe(&m);
        register_slot_scalars(db, SLOT_GEOPOLY, (sqlite_wasm_fts5_slot_manifest_t *)&m);
        sqlite_wasm_geopoly_slot_manifest_free(&m);
    }
    {
        sqlite_wasm_demo_slot_manifest_t m;
        sqlite_wasm_demo_slot_describe(&m);
        register_slot_scalars(db, SLOT_DEMO, (sqlite_wasm_fts5_slot_manifest_t *)&m);
        sqlite_wasm_demo_slot_manifest_free(&m);
    }
    return SQLITE_OK;
}

/* Wire register_all_slots into SQLite's auto-extension chain at
 * component init time. Runs once per process; SQLite invokes the
 * registered fn for every sqlite3_open. */
__attribute__((constructor))
static void wire_slot_registration(void) {
    sqlite3_auto_extension((void (*)(void))register_all_slots);
}
