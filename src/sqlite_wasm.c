/*
 * SQLite WASM Component - Main Integration
 *
 * This file serves as the main entry point for the SQLite WASM component.
 * It handles initialization of the VFS layer and coordinates between
 * the SQLite core and the exported WIT interfaces.
 */

#include <stdint.h>
#include <stdlib.h>
#include "sqlite3.h"

/*
 * VFS registration functions (defined in vfs/*.c)
 */
extern int sqlite3_memvfs_register(int makeDefault);
extern const char *sqlite3_memvfs_name(void);

extern int sqlite3_wasivfs_register(int makeDefault);
extern const char *sqlite3_wasivfs_name(void);

/*
 * Initialization state
 */
static int g_initialized = 0;

/*
 * Custom OS interface for WASM
 *
 * SQLite requires certain OS-level functions to be available.
 * In WASM, we provide stubs or alternative implementations.
 */

/*
 * Required by SQLite when SQLITE_OS_OTHER is defined.
 * This is called by sqlite3_initialize() to set up the OS layer.
 */
int sqlite3_os_init(void) {
    /* Register our VFS implementations */
    int rc = sqlite3_memvfs_register(1);  /* Make default */
    if (rc != SQLITE_OK) {
        return rc;
    }

    rc = sqlite3_wasivfs_register(0);  /* Don't make default */
    /* WASI VFS registration failure is not fatal */

    return SQLITE_OK;
}

/*
 * Required by SQLite when SQLITE_OS_OTHER is defined.
 * Called by sqlite3_shutdown() to clean up the OS layer.
 */
int sqlite3_os_end(void) {
    return SQLITE_OK;
}

/*
 * Initialize the SQLite WASM component
 *
 * This function is called automatically before any SQLite operations
 * via the component model's initialization mechanism.
 *
 * It sets up:
 * 1. The memory VFS (default for in-memory operations)
 * 2. The WASI VFS (for file-based operations when available)
 * 3. SQLite configuration options
 */
__attribute__((constructor))
static void sqlite_wasm_init(void) {
    if (g_initialized) {
        return;
    }

    /*
     * Initialize SQLite. This will call sqlite3_os_init() which
     * registers our VFS implementations.
     */
    int rc = sqlite3_initialize();
    if (rc != SQLITE_OK) {
        /* Initialization failed - we can't do much here in WASM */
        return;
    }

    /*
     * Configure SQLite for WASM environment
     */

    /* Enable serialized threading mode (safest for component model) */
    sqlite3_config(SQLITE_CONFIG_SERIALIZED);

    /* Set a reasonable default cache size */
    /* This will be applied to new connections */

    g_initialized = 1;
}

/*
 * Shutdown handler
 *
 * Clean up SQLite resources when the component is unloaded.
 */
__attribute__((destructor))
static void sqlite_wasm_shutdown(void) {
    if (g_initialized) {
        sqlite3_shutdown();
        g_initialized = 0;
    }
}

/*
 * Health check function
 *
 * Returns 1 if SQLite is properly initialized, 0 otherwise.
 * Useful for debugging and testing.
 */
__attribute__((export_name("sqlite_wasm_is_initialized")))
int sqlite_wasm_is_initialized(void) {
    return g_initialized;
}

/*
 * Get the default VFS name
 *
 * Returns the name of the default VFS (should be "memvfs").
 */
__attribute__((export_name("sqlite_wasm_default_vfs")))
const char *sqlite_wasm_default_vfs(void) {
    sqlite3_vfs *vfs = sqlite3_vfs_find(NULL);
    return vfs ? vfs->zName : "unknown";
}

/*
 * Check if WASI VFS is available
 *
 * Returns 1 if the WASI VFS is registered and available.
 */
__attribute__((export_name("sqlite_wasm_has_wasi_vfs")))
int sqlite_wasm_has_wasi_vfs(void) {
    sqlite3_vfs *vfs = sqlite3_vfs_find(sqlite3_wasivfs_name());
    return vfs != NULL;
}

/*
 * Open a database with explicit VFS selection
 *
 * This allows callers to choose between memvfs and wasivfs.
 * - vfs_name: "memvfs" for in-memory, "wasivfs" for WASI filesystem
 * - filename: database path/name
 * - flags: SQLite open flags
 */
__attribute__((export_name("sqlite_wasm_open_v2")))
int sqlite_wasm_open_v2(const char *filename, sqlite3 **ppDb, int flags, const char *vfs_name) {
    if (!g_initialized) {
        sqlite_wasm_init();
    }
    return sqlite3_open_v2(filename, ppDb, flags, vfs_name);
}

/*
 * SQLite memory allocator wrappers
 *
 * These ensure SQLite uses the WASM linear memory properly.
 * The default sqlite3 malloc/free should work, but these
 * provide explicit control if needed.
 */

__attribute__((export_name("sqlite_wasm_malloc")))
void *sqlite_wasm_malloc(int size) {
    return sqlite3_malloc(size);
}

__attribute__((export_name("sqlite_wasm_free")))
void sqlite_wasm_free(void *ptr) {
    sqlite3_free(ptr);
}

__attribute__((export_name("sqlite_wasm_realloc")))
void *sqlite_wasm_realloc(void *ptr, int size) {
    return sqlite3_realloc(ptr, size);
}

/*
 * Memory status functions
 *
 * Useful for debugging memory usage in the WASM environment.
 */

__attribute__((export_name("sqlite_wasm_memory_used")))
int64_t sqlite_wasm_memory_used(void) {
    int64_t used = 0;
    sqlite3_status64(SQLITE_STATUS_MEMORY_USED, &used, NULL, 0);
    return used;
}

__attribute__((export_name("sqlite_wasm_memory_highwater")))
int64_t sqlite_wasm_memory_highwater(int reset) {
    int64_t highwater = 0;
    sqlite3_status64(SQLITE_STATUS_MEMORY_USED, NULL, &highwater, reset);
    return highwater;
}

/*
 * Version information
 */

__attribute__((export_name("sqlite_wasm_version")))
const char *sqlite_wasm_version(void) {
    return sqlite3_libversion();
}

__attribute__((export_name("sqlite_wasm_version_number")))
int sqlite_wasm_version_number(void) {
    return sqlite3_libversion_number();
}

__attribute__((export_name("sqlite_wasm_sourceid")))
const char *sqlite_wasm_sourceid(void) {
    return sqlite3_sourceid();
}

/*
 * Component identification
 */

__attribute__((export_name("sqlite_wasm_component_version")))
const char *sqlite_wasm_component_version(void) {
    return "0.1.0";
}
