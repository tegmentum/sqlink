/*
 * Compatibility wrapper for extensible build
 * This allows code that includes sqlite_world.h to use the extensible bindings
 */
#ifndef SQLITE_WORLD_COMPAT_H
#define SQLITE_WORLD_COMPAT_H

#include "sqlite_extensible.h"

/* Map sqlite_world types to sqlite_extensible types */
typedef sqlite_extensible_string_t sqlite_world_string_t;
typedef sqlite_extensible_list_u8_t sqlite_world_list_u8_t;
typedef sqlite_extensible_list_string_t sqlite_world_list_string_t;
typedef sqlite_extensible_option_string_t sqlite_world_option_string_t;
typedef sqlite_extensible_option_s64_t sqlite_world_option_s64_t;
typedef sqlite_extensible_option_f64_t sqlite_world_option_f64_t;
typedef sqlite_extensible_option_list_u8_t sqlite_world_option_list_u8_t;

/* String helper functions */
#define sqlite_world_string_set sqlite_extensible_string_set
#define sqlite_world_string_dup sqlite_extensible_string_dup
#define sqlite_world_string_free sqlite_extensible_string_free

#endif /* SQLITE_WORLD_COMPAT_H */
