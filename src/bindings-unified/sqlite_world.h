/*
 * Compatibility wrapper for the unified-WIT build.
 *
 * Maps sqlite_world_* string/list type names that the shared C glue
 * (low_level.c, high_level.c, sqlite_wasm.c) uses to the
 * sqlite_cli_unified_* names generated for the sqlite-cli-unified
 * world. This lets the same .c files compile against three different
 * bindings sets (sqlite-world, sqlite-extensible, sqlite-cli-unified)
 * with only the include path swapped.
 *
 * Export-side types (`exports_sqlite_wasm_*`) keep their names across
 * worlds because the world's exports are package-qualified at the
 * source.
 */
#ifndef SQLITE_WORLD_COMPAT_H
#define SQLITE_WORLD_COMPAT_H

#include "sqlite_cli_unified.h"

typedef sqlite_cli_unified_string_t sqlite_world_string_t;
#define sqlite_world_string_dup sqlite_cli_unified_string_dup
#define sqlite_world_string_free sqlite_cli_unified_string_free
#define sqlite_world_string_set sqlite_cli_unified_string_set

typedef sqlite_cli_unified_list_u8_t sqlite_world_list_u8_t;
typedef sqlite_cli_unified_list_string_t sqlite_world_list_string_t;

#endif /* SQLITE_WORLD_COMPAT_H */
