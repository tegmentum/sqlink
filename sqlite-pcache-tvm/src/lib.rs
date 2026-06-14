//! Custom `sqlite3_pcache_methods2` implementation.
//!
//! ## Layered plan (PLAN-tvm-integration Phase 1)
//!
//! 1. **Phase 1.0 (this code):** the 11 pcache2 trampolines + a
//!    HashMap-backed in-process `Cache` proving the registration
//!    plumbing works end-to-end with real sqlite3.
//! 2. **Phase 1.1 (Path D, shadow-pool):** add a bounded
//!    default-memory shadow pool + TVM-backed cold storage. See
//!    the Phase 1.1 architectural-finding block in
//!    PLAN-tvm-integration.md for the design rationale. The
//!    direct "TVM region returns a raw pointer" backend swap
//!    (originally implied by Phase 1) is structurally
//!    impossible: TVM's > 4 GiB story is multi-memory and SQLite
//!    needs a default-memory C pointer to dereference. Path D
//!    fetches a page from TVM into a shadow slot, returns the
//!    shadow ptr, flushes back on unpin.
//!
//! The current trampolines + lifetime model carry over to
//! Phase 1.1 unchanged — what changes is the `Cache` body: it'll
//! hold a shadow pool + a TVM region handle instead of an
//! unbounded HashMap of owned `PageEntry`s.
//!
//! ## Lifetime model
//!
//! Each `xCreate` allocates a `Cache` on the heap and returns its
//! raw pointer cast to `*mut sqlite3_pcache`. SQLite holds that
//! opaque pointer for the cache's lifetime; `xDestroy` consumes
//! it back into a `Box` and drops it.
//!
//! Each `xFetch` allocates a `PageEntry` on the heap (one tail
//! allocation laid out so `pBuf` points at `szPage` bytes and
//! `pExtra` at the following `szExtra` bytes — SQLite's documented
//! contract). The cache owns the entry; `xUnpin(discard=true)` or
//! `xTruncate` drop it.
//!
//! ## Registration
//!
//! `install()` calls `sqlite3_config(SQLITE_CONFIG_PCACHE2, …)`.
//! SQLite requires that call to land **before** `sqlite3_initialize`,
//! so embedders must call this at the very top of their boot
//! sequence — same constraint that applies to
//! `sqlite3_config(SQLITE_CONFIG_LOG, …)` (see
//! `core::db::install_log_callback` for the analogue pattern).

#![allow(non_snake_case)]

use std::alloc::{alloc, dealloc, Layout};
use std::collections::HashMap;
use std::os::raw::{c_int, c_uint, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use libsqlite3_sys as ffi;
use libsqlite3_sys::{sqlite3_pcache, sqlite3_pcache_methods2, sqlite3_pcache_page};

/// In-process page cache. One instance per SQLite call to xCreate
/// (i.e. one per pager/connection). Holds owned page entries
/// keyed by SQLite's u32 page key.
struct Cache {
    /// Configured page size (sqlite tells us at xCreate).
    sz_page: c_int,
    /// Configured extra-bytes-per-page (sqlite-managed metadata
    /// like dirty bits; we never touch it, just allocate space).
    sz_extra: c_int,
    /// Whether sqlite considers this cache purgeable. We honor it
    /// in xUnpin's `discard` semantics; non-purgeable caches must
    /// retain pages even on unpin.
    purgeable: bool,
    /// Soft cap from xCachesize. Advisory only — sqlite uses it to
    /// hint memory budget; we don't currently enforce eviction at
    /// the cap (the TVM follow-up handles eviction via demote).
    suggested_cap: c_int,
    /// Live pages keyed by page-id (sqlite's xFetch key).
    pages: HashMap<c_uint, *mut PageEntry>,
}

/// One cache entry. Tail-allocated so a single `alloc` call gives
/// us the `sqlite3_pcache_page` header + `pBuf` storage +
/// `pExtra` storage in one contiguous region. SQLite sees only
/// the header pointer; pBuf and pExtra are computed offsets into
/// the tail.
#[repr(C)]
struct PageEntry {
    /// SQLite-visible page handle. pBuf / pExtra are wired to
    /// point into the tail bytes that follow this struct.
    header: sqlite3_pcache_page,
    /// Layout used at alloc, replayed at dealloc to satisfy the
    /// global allocator's matched-alloc/dealloc contract.
    layout: Layout,
    /// True iff this entry is currently fetched (pinned). xUnpin
    /// flips it false; xFetch flips it back true on re-fetch.
    pinned: bool,
}

impl PageEntry {
    /// Allocate a new page entry sized for the given page + extra
    /// payload. Returns a raw pointer; caller (the Cache) owns it
    /// and must drop via `PageEntry::dealloc`.
    fn alloc_zeroed(sz_page: c_int, sz_extra: c_int) -> *mut PageEntry {
        let header_size = std::mem::size_of::<PageEntry>();
        let total = header_size + sz_page as usize + sz_extra as usize;
        // Align to 16 bytes — sqlite doesn't formally require this,
        // but most allocators use it and it matches the alignment
        // libc malloc returns. Conservative.
        let layout = Layout::from_size_align(total, 16).expect("pcache layout");
        // SAFETY: layout has size > 0 (header_size is non-zero).
        let raw = unsafe { alloc(layout) };
        if raw.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        // SAFETY: raw points at uninit memory of at least
        // sizeof(PageEntry) + sz_page + sz_extra bytes, aligned to
        // 16. Zero the whole thing so sqlite sees zeroed page +
        // extra bytes.
        unsafe { std::ptr::write_bytes(raw, 0, total) };
        let entry = raw as *mut PageEntry;
        let pbuf_ptr = unsafe { (raw as *mut u8).add(header_size) };
        let pextra_ptr = unsafe { pbuf_ptr.add(sz_page as usize) };
        // SAFETY: entry points at uninit (zeroed) memory laid out
        // for PageEntry; we initialize all fields directly.
        unsafe {
            std::ptr::write(
                entry,
                PageEntry {
                    header: sqlite3_pcache_page {
                        pBuf: pbuf_ptr as *mut c_void,
                        pExtra: pextra_ptr as *mut c_void,
                    },
                    layout,
                    pinned: true,
                },
            );
        }
        entry
    }

    /// Free a page entry that was allocated by `alloc_zeroed`.
    /// SAFETY: caller must guarantee `entry` is a non-null pointer
    /// from `alloc_zeroed` and that no live references remain.
    unsafe fn dealloc(entry: *mut PageEntry) {
        let layout = (*entry).layout;
        // Drop the PageEntry struct in place to honor any future
        // Drop impl. PageEntry doesn't own anything heap-managed
        // today (its tail bytes share the same allocation), so
        // this is a no-op now and a defensive habit.
        std::ptr::drop_in_place(entry);
        dealloc(entry as *mut u8, layout);
    }
}

impl Cache {
    fn new(sz_page: c_int, sz_extra: c_int, purgeable: bool) -> Self {
        Cache {
            sz_page,
            sz_extra,
            purgeable,
            suggested_cap: 0,
            pages: HashMap::new(),
        }
    }

    /// Number of pages currently held.
    fn page_count(&self) -> c_int {
        self.pages.len() as c_int
    }

    /// Look up a page by key. If absent and `create != 0`, allocate
    /// one. Returns the page header pointer.
    ///
    /// `create_flag`: 0 = don't allocate (return null if absent),
    /// 1 = allocate if cache has budget, 2 = allocate even if over
    /// budget. We ignore the budget hint today.
    fn fetch(&mut self, key: c_uint, create_flag: c_int) -> *mut sqlite3_pcache_page {
        if let Some(&entry) = self.pages.get(&key) {
            // SAFETY: we own the pointer; only one fetcher at a
            // time per SQLite's threading contract for this cache.
            unsafe { (*entry).pinned = true };
            return unsafe { &mut (*entry).header };
        }
        if create_flag == 0 {
            return ptr::null_mut();
        }
        let entry = PageEntry::alloc_zeroed(self.sz_page, self.sz_extra);
        self.pages.insert(key, entry);
        unsafe { &mut (*entry).header }
    }

    /// SAFETY: `page` must be a header pointer returned from
    /// `fetch` against this cache and not yet unpinned-and-discarded.
    unsafe fn unpin(&mut self, page: *mut sqlite3_pcache_page, discard: bool) {
        // The entry pointer is the header pointer (header is the
        // first field of PageEntry, #[repr(C)]).
        let entry = page as *mut PageEntry;
        (*entry).pinned = false;
        if discard || self.purgeable {
            // Walk to find which key this entry held so we can
            // remove it. The map is small (cache_size pages); a
            // linear scan is fine for first commit. The TVM swap
            // can introduce a back-pointer if profiling shows it.
            let to_remove: Option<c_uint> = self
                .pages
                .iter()
                .find_map(|(k, &v)| if v == entry { Some(*k) } else { None });
            if let Some(k) = to_remove {
                self.pages.remove(&k);
                PageEntry::dealloc(entry);
            }
        }
    }

    /// SAFETY: `page` must be a header pointer returned from
    /// `fetch` against this cache.
    unsafe fn rekey(&mut self, page: *mut sqlite3_pcache_page, old_key: c_uint, new_key: c_uint) {
        let entry = page as *mut PageEntry;
        // If new_key already has an entry, the old one must be
        // dropped per pcache2 contract.
        if let Some(existing) = self.pages.remove(&new_key) {
            if existing != entry {
                PageEntry::dealloc(existing);
            }
        }
        self.pages.remove(&old_key);
        self.pages.insert(new_key, entry);
    }

    /// Drop every page whose key is >= `limit`.
    fn truncate(&mut self, limit: c_uint) {
        let to_remove: Vec<c_uint> = self
            .pages
            .keys()
            .copied()
            .filter(|k| *k >= limit)
            .collect();
        for k in to_remove {
            if let Some(entry) = self.pages.remove(&k) {
                // SAFETY: entry came from our HashMap, allocated by
                // PageEntry::alloc_zeroed; no live refs remain
                // after the remove.
                unsafe { PageEntry::dealloc(entry) };
            }
        }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        for (_, entry) in self.pages.drain() {
            // SAFETY: entries were allocated by PageEntry::alloc_zeroed
            // and only stored here.
            unsafe { PageEntry::dealloc(entry) };
        }
    }
}

// ---------------------------------------------------------------
// Trampolines.
// SQLite calls these with raw pointers; each trampoline casts back
// to &mut Cache (or PageEntry) and forwards to the impl above.
// ---------------------------------------------------------------

unsafe extern "C" fn x_init(_arg: *mut c_void) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_shutdown(_arg: *mut c_void) {
    // No global state to release.
}

unsafe extern "C" fn x_create(
    sz_page: c_int,
    sz_extra: c_int,
    purgeable: c_int,
) -> *mut sqlite3_pcache {
    let cache = Box::new(Cache::new(sz_page, sz_extra, purgeable != 0));
    Box::into_raw(cache) as *mut sqlite3_pcache
}

unsafe extern "C" fn x_cachesize(p: *mut sqlite3_pcache, n: c_int) {
    let cache = &mut *(p as *mut Cache);
    cache.suggested_cap = n;
}

unsafe extern "C" fn x_pagecount(p: *mut sqlite3_pcache) -> c_int {
    let cache = &*(p as *const Cache);
    cache.page_count()
}

unsafe extern "C" fn x_fetch(
    p: *mut sqlite3_pcache,
    key: c_uint,
    create_flag: c_int,
) -> *mut sqlite3_pcache_page {
    let cache = &mut *(p as *mut Cache);
    cache.fetch(key, create_flag)
}

unsafe extern "C" fn x_unpin(
    p: *mut sqlite3_pcache,
    page: *mut sqlite3_pcache_page,
    discard: c_int,
) {
    let cache = &mut *(p as *mut Cache);
    cache.unpin(page, discard != 0)
}

unsafe extern "C" fn x_rekey(
    p: *mut sqlite3_pcache,
    page: *mut sqlite3_pcache_page,
    old_key: c_uint,
    new_key: c_uint,
) {
    let cache = &mut *(p as *mut Cache);
    cache.rekey(page, old_key, new_key)
}

unsafe extern "C" fn x_truncate(p: *mut sqlite3_pcache, limit: c_uint) {
    let cache = &mut *(p as *mut Cache);
    cache.truncate(limit)
}

unsafe extern "C" fn x_destroy(p: *mut sqlite3_pcache) {
    // SAFETY: p came from `x_create`'s Box::into_raw. Reclaim it.
    drop(Box::from_raw(p as *mut Cache));
}

unsafe extern "C" fn x_shrink(_p: *mut sqlite3_pcache) {
    // Best-effort eviction hint. We don't enforce a budget today,
    // so there's nothing to shrink.
}

/// Sync newtype around the methods table. `sqlite3_pcache_methods2`
/// holds a `*mut c_void` (`pArg`) so it's not auto-Sync; the value
/// we use is `null_mut()` and the table is otherwise read-only
/// after construction, so a manual `unsafe impl Sync` is sound.
#[repr(transparent)]
struct MethodsTable(sqlite3_pcache_methods2);
unsafe impl Sync for MethodsTable {}

/// The methods table we register with SQLite. `iVersion=1` matches
/// the field set above (pcache_methods2 v1 didn't yet add `pArg`
/// extension functions in v2; we don't use any of them).
static METHODS: MethodsTable = MethodsTable(sqlite3_pcache_methods2 {
    iVersion: 1,
    pArg: ptr::null_mut(),
    xInit: Some(x_init),
    xShutdown: Some(x_shutdown),
    xCreate: Some(x_create),
    xCachesize: Some(x_cachesize),
    xPagecount: Some(x_pagecount),
    xFetch: Some(x_fetch),
    xUnpin: Some(x_unpin),
    xRekey: Some(x_rekey),
    xTruncate: Some(x_truncate),
    xDestroy: Some(x_destroy),
    xShrink: Some(x_shrink),
});

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Register this crate's pcache2 impl as SQLite's page cache. Must
/// run *before* `sqlite3_initialize` per SQLite's
/// `SQLITE_CONFIG_PCACHE2` contract (any later change requires
/// `sqlite3_shutdown` first).
///
/// Safe to call multiple times — subsequent calls are no-ops
/// (sqlite3_config rejects re-registration after initialize, and
/// our atomic gate avoids surprising error returns).
pub fn install() -> Result<(), InstallError> {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    // SAFETY: `&METHODS` is a static reference to a `'static`
    // table sqlite holds for the program lifetime. sqlite3_config
    // copies the iVersion, then keeps the pointer for callbacks.
    let rc = unsafe {
        ffi::sqlite3_config(
            ffi::SQLITE_CONFIG_PCACHE2,
            &METHODS.0 as *const _ as *const c_void,
        )
    };
    if rc != ffi::SQLITE_OK {
        // Failed register — flip the gate back so a corrected
        // boot order can try again.
        INSTALLED.store(false, Ordering::SeqCst);
        return Err(InstallError {
            code: rc,
            message: "sqlite3_config(SQLITE_CONFIG_PCACHE2) failed; \
                      must be called before sqlite3_initialize"
                .to_string(),
        });
    }
    Ok(())
}

/// Failure shape returned by `install`. The numeric code is the
/// raw SQLite return; the message names which boot-order
/// constraint was likely violated.
#[derive(Debug)]
pub struct InstallError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for InstallError {}
