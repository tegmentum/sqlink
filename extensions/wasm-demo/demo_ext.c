/*
 * sqlite:wasm-demo demonstration extension.
 *
 * Implements the sqlite:wasm/demo-slot interface end-to-end. Composed
 * with sqlite-unified.wasm via wac, this proves the slot-driven
 * dispatch architecture works: the host calls describe() at startup,
 * registers wasm_reverse + wasm_double with SQLite via
 * sqlite3_create_function_v2, and routes SQL invocations back through
 * scalar-function.call() to the implementations below.
 */

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "demo_extension.h"

#define FUNC_REVERSE 1u
#define FUNC_DOUBLE  2u

/* Helper: malloc + copy a null-terminated literal into wit-bindgen
 * string layout (ptr/len, NOT null-terminated). The cabi runtime owns
 * the memory after we return. */
static void set_lit(demo_extension_string_t *out, const char *s) {
    size_t n = strlen(s);
    out->ptr = (uint8_t *)malloc(n);
    if (out->ptr) {
        memcpy(out->ptr, s, n);
        out->len = n;
    } else {
        out->len = 0;
    }
}

/* Build a manifest describing the demo functions. */
void exports_sqlite_wasm_demo_slot_describe(
    exports_sqlite_wasm_demo_slot_manifest_t *ret
) {
    /* manifest.name */
    set_lit(&ret->name, "wasm-demo");
    /* manifest.version */
    set_lit(&ret->version, "0.1.0");

    /* manifest.scalar_functions: { wasm_reverse, wasm_double }.
     * The spec type is package-level (sqlite_extension_metadata_*),
     * not per-slot, because the slot interface uses
     * `use sqlite:extension/metadata.{manifest}`. */
    sqlite_extension_metadata_scalar_function_spec_t *specs =
        (sqlite_extension_metadata_scalar_function_spec_t *)malloc(2 * sizeof(*specs));

    specs[0].id = FUNC_REVERSE;
    set_lit(&specs[0].name, "wasm_reverse");
    specs[0].num_args = 1;
    specs[0].func_flags = 1; /* deterministic */

    specs[1].id = FUNC_DOUBLE;
    set_lit(&specs[1].name, "wasm_double");
    specs[1].num_args = 1;
    specs[1].func_flags = 1; /* deterministic */

    ret->scalar_functions.ptr = specs;
    ret->scalar_functions.len = 2;

    /* No aggregates, collations, hooks. */
    ret->aggregate_functions.ptr = NULL;
    ret->aggregate_functions.len = 0;
    ret->collations.ptr = NULL;
    ret->collations.len = 0;
    ret->has_authorizer = false;
    ret->has_update_hook = false;
    ret->has_commit_hook = false;

    /* No host capabilities declared — wasm_reverse/wasm_double are
     * pure functions. */
    ret->declared_capabilities.ptr = NULL;
    ret->declared_capabilities.len = 0;
}

/* Returns an error variant via the wit-bindgen calling convention. */
static bool fail(demo_extension_string_t *err, const char *msg) {
    set_lit(err, msg);
    return false;
}

/* Empty-manifest helper for the stub slots. The host iterates these
 * but finds no functions to register, so the stubs are inert at
 * runtime — they exist only to satisfy the WIT-level import-export
 * pairing wac requires for full composition. */
static void empty_manifest(sqlite_extension_metadata_manifest_t *m,
                           const char *name) {
    set_lit(&m->name, name);
    set_lit(&m->version, "0.0.0");
    m->scalar_functions.ptr = NULL;
    m->scalar_functions.len = 0;
    m->aggregate_functions.ptr = NULL;
    m->aggregate_functions.len = 0;
    m->collations.ptr = NULL;
    m->collations.len = 0;
    m->has_authorizer = false;
    m->has_update_hook = false;
    m->has_commit_hook = false;
    m->declared_capabilities.ptr = NULL;
    m->declared_capabilities.len = 0;
}

void exports_sqlite_wasm_fts5_slot_describe(sqlite_extension_metadata_manifest_t *ret) {
    empty_manifest(ret, "fts5-stub");
}
bool exports_sqlite_wasm_fts5_slot_call(uint64_t func_id,
                                        exports_sqlite_wasm_fts5_slot_list_sql_value_t *args,
                                        exports_sqlite_wasm_fts5_slot_sql_value_t *ret,
                                        demo_extension_string_t *err) {
    (void)func_id; (void)args; (void)ret;
    return fail(err, "fts5-stub has no functions");
}

void exports_sqlite_wasm_json1_slot_describe(sqlite_extension_metadata_manifest_t *ret) {
    empty_manifest(ret, "json1-stub");
}
bool exports_sqlite_wasm_json1_slot_call(uint64_t func_id,
                                         exports_sqlite_wasm_json1_slot_list_sql_value_t *args,
                                         exports_sqlite_wasm_json1_slot_sql_value_t *ret,
                                         demo_extension_string_t *err) {
    (void)func_id; (void)args; (void)ret;
    return fail(err, "json1-stub has no functions");
}

void exports_sqlite_wasm_rtree_slot_describe(sqlite_extension_metadata_manifest_t *ret) {
    empty_manifest(ret, "rtree-stub");
}
bool exports_sqlite_wasm_rtree_slot_call(uint64_t func_id,
                                         exports_sqlite_wasm_rtree_slot_list_sql_value_t *args,
                                         exports_sqlite_wasm_rtree_slot_sql_value_t *ret,
                                         demo_extension_string_t *err) {
    (void)func_id; (void)args; (void)ret;
    return fail(err, "rtree-stub has no functions");
}

void exports_sqlite_wasm_geopoly_slot_describe(sqlite_extension_metadata_manifest_t *ret) {
    empty_manifest(ret, "geopoly-stub");
}
bool exports_sqlite_wasm_geopoly_slot_call(uint64_t func_id,
                                           exports_sqlite_wasm_geopoly_slot_list_sql_value_t *args,
                                           exports_sqlite_wasm_geopoly_slot_sql_value_t *ret,
                                           demo_extension_string_t *err) {
    (void)func_id; (void)args; (void)ret;
    return fail(err, "geopoly-stub has no functions");
}

bool exports_sqlite_wasm_demo_slot_call(
    uint64_t func_id,
    exports_sqlite_wasm_demo_slot_list_sql_value_t *args,
    exports_sqlite_wasm_demo_slot_sql_value_t *ret,
    demo_extension_string_t *err
) {
    if (args->len < 1) {
        return fail(err, "missing argument");
    }
    exports_sqlite_wasm_demo_slot_sql_value_t *arg0 = &args->ptr[0];

    switch (func_id) {
        case FUNC_REVERSE: {
            if (arg0->tag != SQLITE_EXTENSION_TYPES_SQL_VALUE_TEXT) {
                return fail(err, "wasm_reverse expects text");
            }
            size_t n = arg0->val.text.len;
            uint8_t *src = arg0->val.text.ptr;
            uint8_t *dst = (uint8_t *)malloc(n);
            if (!dst) return fail(err, "out of memory");
            for (size_t i = 0; i < n; i++) {
                dst[i] = src[n - 1 - i];
            }
            ret->tag = SQLITE_EXTENSION_TYPES_SQL_VALUE_TEXT;
            ret->val.text.ptr = dst;
            ret->val.text.len = n;
            return true;
        }
        case FUNC_DOUBLE: {
            if (arg0->tag != SQLITE_EXTENSION_TYPES_SQL_VALUE_INTEGER) {
                return fail(err, "wasm_double expects integer");
            }
            ret->tag = SQLITE_EXTENSION_TYPES_SQL_VALUE_INTEGER;
            ret->val.integer = arg0->val.integer * 2;
            return true;
        }
        default:
            return fail(err, "unknown function id");
    }
}
