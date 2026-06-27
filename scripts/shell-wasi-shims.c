/* WASI shims for the real sqlite3 shell built to wasm32-wasip2.
 *
 * WASI (preview 2) has no subprocess model, so the shell's
 * .shell / .system / "open with" dot-commands — which shell out via
 * system() — have nothing to call. Stub system() to a clean ENOSYS
 * failure so those commands report an error at runtime instead of
 * failing to LINK. Core SQL, every output mode, and all other
 * dot-commands are unaffected.
 *
 * Everything else the shell needs that POSIX lacks on WASI (signals,
 * process clocks, getpid) is supplied by the wasi-sdk emulation libs
 * (-lwasi-emulated-signal / -process-clocks / -getpid) wired in
 * build-shell-wasm.sh; only system() has no emulation lib, hence this
 * one shim.
 */
#include <errno.h>

int system(const char *command) {
    (void)command;
    errno = ENOSYS;
    return -1;
}
