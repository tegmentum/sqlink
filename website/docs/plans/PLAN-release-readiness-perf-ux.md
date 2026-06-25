# PLAN: release-readiness perf + UX audit

Read-only audit of the workspace ahead of OSS release. No code
changed; this is the punch list.

---

## TL;DR — 8 highest-value wins (top of the page)

| # | Win | Effort | Impact | Do this |
|---|---|---|---|---|
| 1 | **Replace hand-rolled argv parsing with `clap`.** `host/src/main.rs` is 770 lines of manual `match args[i]` over 6 subcommands. `sqlink --help` today is a literal `eprintln!` of usage strings (lines 46-53). | M | dev-ergo + UX | Adopt `clap` derive; one struct per subcommand. Auto-generated help, validation, completions for free. |
| 2 | **Cap the `host/src/lib.rs` mega-module.** 10,189 LOC in one file is the single biggest readability/contribution barrier in the repo. Stages 5e/6 grew it ~3000 lines. | L | dev-ergo + maintainability | Split into `host/src/loader.rs`, `host/src/spi_host.rs`, `host/src/vtab.rs` (already exists), `host/src/session_ffi.rs` (already exists), `host/src/dispatch.rs`. Target ≤3000 LOC per file. |
| 3 | **Pre-allocate row vectors in `execute_multi_impl_*`.** Both copies (host/src/lib.rs:3451 + 8814) do `Vec::new()` for `results` and `out_rows`. For multi-statement scripts and bulk inserts, `Vec::with_capacity(stmt_count)` and `Vec::with_capacity(column_count * row_count_estimate)` save N realloc copies. | S | perf | `let mut results = Vec::with_capacity(8);` plus pre-size `out_rows` to `rows.len()`. |
| 4 | **Trim `cli/Cargo.toml`'s 89 `embed-*` features.** Default build has none enabled, but every feature decl + 89 optional path deps is in the manifest. Touching `cli/Cargo.toml` invalidates the cli build incrementally; the 89 dep lines also slow `cargo metadata` and IDE-side path resolution. | M | size + dev-ergo | Move the embed-* feature wall behind a `tooling/cli-features.toml` consumed by `sqlink compose --embed`. cli/Cargo.toml stays ~30 lines. |
| 5 | **Error messages drop file path context.** `cli/src/lib.rs:875,1550,1565` return bare `Error: {message}` with no command name or input path. A user typing `.serialize /tmp/foo.db` gets `Error: file exists` with no indication of WHICH command. | S | UX | Prefix every cli error with the dot-command name + arg: `Error: .serialize /tmp/foo.db: file exists`. |
| 6 | **Move 89 + extension Cargo.toml descriptions to a sidecar JSON.** Provenance scanner re-parses 200+ Cargo.toml files on every `make ext`; ~half of them are 5-10-line block descriptions. Heavy build noise for catalog metadata. | M | size + scanner speed | Track only the canonical name+version in Cargo.toml; move long-form description into `provenance/descriptions.json`. | 
| 7 | **2.3 MB cli component still 5x the host code's "should be" target.** Stage 5f dropped 2.4 MB → 1.3 MB, but ~1 MB grew back with the dotcmd-aware extension embedding + session-cli + sqlite-utils-*. Confirm via `wasm-tools component sizes target/wasm32-wasip2/release/sqlite_cli.component.wasm`. | M | size | Audit included extensions; lazy-load instead of embed for the rarely-used (archive-cli, session-cli). |
| 8 | **Comprehensive test pass: there are no e2e CI smokes for the .sqlink registry path.** `tests/cli/` has a wrapper; `examples/sqlite-utils-tour.sql` covers the utils family; but `.sqlink list/install/uninstall/verify` has no fixture. | M | release-safety | Add `tests/cli/sqlink-meta-tour.sql` mirroring sqlite-utils-tour.sh's shape. |

**Headline win — #1 (clap migration).** Best effort-to-release-day-impact ratio: a single afternoon of work upgrades the binary from "looks like a 90s C tool" to a recognizable Rust CLI surface. Auto-generated `--help`, `--version`, error messages with command context, and shell completions — all from one struct hierarchy. Every user's first interaction with the project is `sqlink --help`, and right now that interaction emits eight raw `eprintln!` lines with no subcommand discoverability.

---

## 1. Binary size inventory

| Artifact | Size | Baseline | Notes |
|---|---|---|---|
| `target/release/sqlink` (native host) | **28.3 MB** | — (not tracked) | Includes wasmtime + sqlite + tokio. Large but reasonable for a Rust binary embedding wasmtime + libsqlite3 + reqwest. Stripping (`strip target/release/sqlink`) probably halves it. |
| `target/wasm32-wasip2/release/sqlite_cli.component.wasm` | **2.26 MB** | 1.3 MB post-Stage-5f | **Regressed by ~1 MB** — the dotcmd extension embed (core-dotcmd + 10 dotcmd-aware components) re-grew the cli. Stage 5f's 1.3 MB number was *before* dotcmd extensions were auto-embedded. |
| `sqlite_cli.wasm` (raw module) | 2.29 MB | — | Slightly larger than the component — the component encoding compresses some metadata; the raw .wasm is the source. |
| `sqlite_cli_embedded.component.wasm` | 2.44 MB | — | Sibling artifact at workspace target/; unclear if still built by current `make`. Investigate whether it's stale. |

**Top 10 extension components (in extensions/`<name>`/target/):**

| Extension | Size | Why |
|---|---|---|
| onnx | 17.0 MB | ONNX runtime ships statically. Expected — ML model loading. |
| parquet | 8.6 MB | Apache Arrow C++ stack via wasm. Expected. |
| compress | 3.4 MB | bzip2 + lz4 + zstd + miniz_oxide all-bundled. Could split per-codec. |
| bpe | 3.2 MB | tokenizers-rs + tiktoken. Unsplit. |
| sqlparse | 3.2 MB | sqlparse-rs full grammar. |
| phone | 2.3 MB | libphonenumber via wasm. Country-specific data tables. |
| arrow | 2.2 MB | Apache Arrow read-only path. |
| web-parsers | 1.9 MB | HTML + CSS + URL parsers (kuchikiki + scraper). |
| sentiment | 1.8 MB | Lexicon-based; the 1.8 MB is the dictionary blob. |
| useragent | 1.5 MB | uap-rs regex table. |

**Unexpectedly fat:**
- `onnx` and `parquet` are by design but should never auto-embed.
- `compress`: 3.4 MB for what could be 4× ~800 KB sub-extensions; split is reasonable for users who only need zstd.
- `sqlparse`: 3.2 MB for a parser — could be a sign that the bundled grammar pulls in features unused by current callers.

---

## 2. Allocation hot spots (top 10 with citations)

| # | File:line | Pattern | Cost |
|---|---|---|---|
| 1 | `host/src/lib.rs:3451,8814` | Both `execute_multi_impl_*` start with `let mut results = Vec::new()` and `let mut out_rows: Vec<Vec<_>> = ...collect()`. No capacity hint despite knowing column count from `stmt.column_names()` upstream. | Per multi-statement script: N reallocs of `results`, M reallocs of `out_rows` for M rows. |
| 2 | `host/src/lib.rs:1734` | `register_host_loaded_scalar`'s `xfunc` trampoline: `Vec::with_capacity(argc)` — good. But every invocation also boxes a fresh `String` for `ext_name` lookup via `&scalar_ctx.ext_name`. | Acceptable; the alloc is for a heap pointer copy, not the string. |
| 3 | `host/src/lib.rs` (125 `.clone()` calls) | `let policy = self.host.trust_policy.read().clone();` (line 548), and similar `Arc::clone` + `String::clone` in component-cache + extension-load paths. | Cold path; not measurable per-call. Worth a `Cow<'_, str>` review on hot iterators. |
| 4 | `host/src/lib.rs` (24× `Vec::new()` vs 1× `Vec::with_capacity`) | Imbalance suggests Vec::new is the default reach. | Cold for most; the multi-statement loop (#1) is the lone hot one. |
| 5 | `cli/src/lib.rs` (155 `format!` calls, 18 `.clone()`) | Many in error paths (fine). But `do_help` (`cli/src/lib.rs:601-603`) `format!`s once per dot command in the 73-row enumeration: `out.push_str(&format!("  .{:<width$}  {}\n", n, s, width = max_name));`. | One-shot `.help` invocation; 73 small alloc-and-free cycles is cheap but `write!` against a `&mut String` would skip the intermediate `String` per row. |
| 6 | `extensions/sqlite-utils-data/src/lib.rs:267,286,293` | Arg parser: `flags.insert(rest.to_string(), toks[i + 1].clone())` plus `positionals.push(t.clone())`. The `toks` array is owned; cloning into containers means double allocation. | Per `.insert` / `.upsert` invocation; not per row, so cold. |
| 7 | `core/src/db.rs:861-891` (`bind`) | `Value::Text(s)` binds with `SQLITE_TRANSIENT` which makes sqlite3 copy. For known-immutable strings (the param values in execute_multi), `SQLITE_STATIC` would let sqlite reference the caller's memory and skip the copy. | Per param bind; could be 5-50% win on bulk inserts depending on text length. Needs benchmark. |
| 8 | `core/src/db.rs:892-905` (`bind_all`) | Loops calling `bind` per value, which through #7 hits N transient copies. Could vectorize: single allocation arena + STATIC binds. | Same as #7. |
| 9 | `host/src/lib.rs` `convert_sql_value_to_loaded` / `_from_loaded` paths | Two bindgen universes (`loaded::sqlite::extension::types` vs `bindings::sqlite::extension::types`) force value-by-value translation; each `Text(String)` and `Blob(Vec<u8>)` is moved between owners but the types are structurally identical. | Per scalar/aggregate dispatch; not hot in interactive workloads but real for bulk vtab streams. |
| 10 | `cli/src/lib.rs:147` (`.read FILE` loop) | `format!(".load {}", path)` inside the per-line loop. Acceptable but `write!` into a reused String would scale better for big .sql scripts. | One alloc per .load directive in a .read'd file. Cold for human workflows; hot for CI fixtures. |

**Pattern summary:** The codebase has a clear `Vec::new()` + grow default. A workspace-wide policy ("use `Vec::with_capacity` when the size is known within ±1") would catch ~80% of these without touching cold code.

---

## 3. Wasm component bloat surface

`cli/Cargo.toml` has:
- **2 direct deps** (wit-bindgen, wit-bindgen-rt) — required.
- **2 small deps** (sha3, blake3, miniz_oxide) — used by the cli's own SHA3 + CAS-resolver paths. Each ~80 KB.
- **89 optional `extension` path deps**, all `default-features = false` and behind `embed-*` feature flags. **None enabled by default.** The wasm size grows only when a downstream build opts in via `--features embed-X`.

**Real cli wasm bloat sources** (default build, no embed-* on):
- **Auto-embedded dotcmd extensions** — the cli ships with 10 `dotcmd-aware` components baked in via `include_bytes!` (core-dotcmd 360 KB, sqlite-utils-* family ~600 KB total, session-cli ~30 KB, archive-cli ~250 KB, serialize-cli ~30 KB, sha3sum-cli ~30 KB, sqlink-meta-cli ~100 KB). **~1.4 MB total** — accounts for ~60% of the current 2.26 MB.
- **wit-bindgen-generated code** for the worlds the cli imports — every interface the cli ever calls (spi.* + extension-loader + cli-state + session + dispatch + wasm-slots) gets a Host trait + a generated module. Some of these are dead-code-eliminable if the cli stops importing the ones it doesn't use (e.g. cli imports `transaction` from spi but never calls `spi.transaction.begin()`).

**Estimated trims:**
- Lazy-load archive-cli + sqlink-meta-cli instead of embed: **~350 KB drop**.
- Drop the `wasm-slots` world from the cli (compose-only, not cli) if it's still imported: **~60 KB** (needs verification).
- Run `wasm-opt -Os` on the final component: **~5-10% additional**.

---

## 4. Hot path profile pointers

### a) `host/src/lib.rs::execute_multi_impl_bindings` (8814)
- `Vec::new()` for `results` + `out_rows`. See #1 in allocation hot spots.
- `named_params.iter().find(|p| p.name == bare)` — **O(N) lookup per parameter, called inside per-statement loop**. For a statement with 10 params bound from a list of 50 named params, that's 500 comparisons. Build a `HashMap<&str, &SqlValue>` once outside the statement loop.
- Calls `conn.changes()` + `conn.last_insert_rowid()` per statement *unconditionally* — even for SELECTs that don't modify anything. Two FFI calls per statement. Could short-circuit when statement is read-only (cheap to detect from `sqlite3_stmt_readonly`).

### b) Scalar dispatch (`host/src/lib.rs:1734-1810`)
- Per call: 1 `Vec::with_capacity(argc)` (good), 1 alloc per text/blob arg via `sqlite3_value_to_bindings`. **No pooling.** A SELECT that fires the same scalar 1M times allocates 1M arg vecs.
- `sync_dispatch_scalar` then hands the args to wasmtime via async block_on. The wasmtime call itself dwarfs the alloc cost — unless the wasm side is trivial (single hash, single integer op), in which case the alloc is real.
- **Needs benchmark.** Hypothesis: bulk `SELECT my_scalar(col)` over 1M rows spends 30%+ in alloc.

### c) vtab xColumn (`host/src/vtab.rs`)
- Each xColumn call: trampoline → async dispatch → bindgen value convert → sqlite3_result_* write. **4 hops per cell.** For a 10-col 100k-row scan, that's 4M boundary crossings.
- The `fetch_batch` path (PLAN-perf-rollout.md Phase A) was supposed to amortize this; verify that the 8 read-only vtabs that opted into batch are actually hitting the batched path under `.mode column SELECT * FROM big_vtab` workloads. **Needs benchmark.**

### d) cli's `.help` walk (`cli/src/lib.rs:do_help`)
- Walks `extension_loader::list_extensions()` — one WIT call returning 10 manifests. Each manifest carries all dot-commands (73 total). Builds a `Vec<(String, String, String)>` then sorts.
- Per-row `format!("  .{:<width$}  {}\n", ...)`. As noted in #5: use `write!`.
- One-shot; not hot. But the implementation is the model for how `.help <cmd>` works (also walks the whole list to find one) — that should be a HashMap.

### e) `core/src/db.rs::prepare_with_tail`
- `CString::new(sql)` per call. Each statement in a multi-statement script does this even though the original `&str` is contiguous. Could use `sqlite3_prepare_v3` with an explicit byte-length to avoid `CString` allocation. **Needs benchmark.**

---

## 5. `sqlink --help` quality

`host/src/main.rs:46-53` hand-rolls help via `eprintln!`. Eight lines, no subcommand grouping, no descriptions of what each does, no examples.

**Current output (paraphrased):**
```
usage: sqlink [--db PATH] [--cache-dir DIR] [--no-component-cache] <component.wasm|.cwasm> [-- guest-args...]
       sqlink changeset {invert|concat} <in1> [in2] <out>
       sqlink changeset capture --db PATH --sql FILE --output FILE [--table NAME]
       sqlink changeset apply --db PATH --input FILE
       sqlink precompile <in.wasm> <out.cwasm>
       sqlink compose --list
       sqlink compose --embed NAME[,NAME...] [--output PATH] [--precompile] [--repo-root DIR]
```

**Issues:**
- (a) accurate: ✅ shows all known subcommands.
- (b) covers flags: ⚠️ flags like `--trust=manifest|stored|prompt` and `--exec` (for one-shot cli scripts) aren't here.
- (c) gives examples: ❌ none.
- `sqlink --help` itself works (triggered at line 543), but `sqlink changeset --help` does not exist; the user gets the global help instead.
- No `sqlink <subcommand> --help` routing.
- No `--version`.

**`cli/src/lib.rs::do_help`** is dynamic and correct — enumerates all 73 dot-commands. Solid.

---

## 6. Error message quality (10 samples, top 5 worst)

| # | Site | Current | Problem | Suggested |
|---|---|---|---|---|
| 1 | `cli/src/lib.rs:875` (`do_trace`) | `Err(e) => format!("Error: {}\n", e.message)` | No context: which command failed? Just "Error: foo". | `format!("Error: .trace: {}\n", e.message)` |
| 2 | `cli/src/lib.rs:561` | `format!("Unknown command: {trimmed}\n")` | Doesn't suggest `.help` for discovery. User has no next step. | `format!("Unknown command: {trimmed}\nRun .help to list available commands.\n")` |
| 3 | `cli/src/lib.rs:984` (`.read`) | `format!("Error: cannot open {arg}: {e}\n")` | OK but `{e}` is `std::io::Error` debug, e.g. "No such file or directory (os error 2)" — verbose. | Surface the underlying `kind()` and skip the `(os error 2)` postfix; `.read FILE: file not found` is enough. |
| 4 | `cli/src/lib.rs:1057` | `format!("Bad flag: {arg} (expected --key=value)\n")` | Doesn't say WHICH command was being .loaded; user invoking `.load foo.wasm --trustt=stored` (typo) sees just "Bad flag: --trustt=stored". | `format!(".load: bad flag {arg} (expected --key=value, e.g. --trust=stored)\n")` |
| 5 | `cli/src/lib.rs:1162,1202,1246,1251` (`.load`) | `format!("Error describing {path}: {} (code {})\n", e.message, e.code)` | The "(code N)" sqlite code is jargon; users don't know what code 1 means. Drop the parenthetical OR translate well-known codes to names. | `format!("Error: .load {path}: {} [SQLITE_ERROR]\n", e.message)` for code=1, etc. |

**Pattern:** errors describe the failure but not the user's invocation. A consistent prefix (`.<command> <arg>:`) would fix 60% of the 50+ error sites.

---

## 7. Dev ergonomics

**Clone-to-running-binary path:**
```
git clone <repo>
cd sqlink
git submodule update --init   # sqlite-loader-wit
# Install wasi-sdk if missing (CONTRIBUTING points at the release page)
bash scripts/setup-cargo-config.sh
cargo build --release                          # ~6 min cold (wasmtime + sqlite)
cargo build -p sqlite-cli --target wasm32-wasip2 --release  # ~1 min
wasm-tools component new ...                   # ~1 sec
./target/release/sqlink --db /tmp/x.db target/wasm32-wasip2/release/sqlite_cli.component.wasm
```

**Estimated first-clone-to-prompt: ~10-12 minutes** on a modern machine with a warm Rust toolchain. **Half of that is the wasmtime native dep.**

**Friction points:**
1. **wasi-sdk discovery is multi-step.** CONTRIBUTING points at the wasi-sdk releases page, then the user downloads, extracts, places it somewhere the script can find, runs the script. **Could ship a `scripts/install-wasi-sdk.sh` that fetches the right tarball.** Effort: S; impact: significantly reduces "ugh, what's wasi-sdk" first-touch friction.
2. **The `wasm-tools` requirement is implicit.** README's "Quick taste" section uses `wasm-tools component new ...` but `wasm-tools` is not a Rust workspace dep — the user needs `cargo install wasm-tools` separately. **Document this in CONTRIBUTING.**
3. **`cargo build` from the workspace root builds only `core` + `host` (`default-members`).** New contributors will run `cargo build`, see it succeed, then be confused why the cli component isn't built. **Add a `make all` or `scripts/build-all.sh` that builds everything in the right order.** Effort: S.
4. **`tooling/cli-smoke.py` + `tooling/smoke.py` docstrings still describe behavior accurately** (verified by reading the top of each). ✅
5. **`tooling/bench.py` references PLAN-benchmarks.md** — that path is now `docs/plans/PLAN-benchmarks.md` post-reorg. **Update.** Effort: trivial.
6. **`scripts/setup-cargo-config.sh` does NOT detect the wasi-sdk version mismatch.** If the user installed wasi-sdk-22 but the project expects wasi-sdk-24+, the build will fail with cryptic linker errors. Detection: check the wasi-sdk version against a known minimum. Effort: S.
7. **17 TODO/FIXME markers** across the codebase. Sample:
   - `host/src/lib.rs`: `// TODO: gate by policy.fs once a filesystem capability lands`
   - The rest are mostly `FID_TODO_COUNT` (extensions/ical/) — those are field IDs, not actual TODOs.
   Real TODOs: ~3. Acceptable.

---

## 8. Comprehensive testing gaps

- **e2e CI:** `.github/workflows/ci.yml` runs `cargo test` + builds; no integration test of the full sqlink+component-wasm path. **Add a CI step that builds the cli component and runs `examples/sqlite-utils-tour.sql` against it.**
- **`.sqlink` registry surface (sqlink-meta-cli) has NO smoke fixture.** The 9 subcommands (`list/show/install/uninstall/bundle/unbundle/verify/gc/export`) are reachable via `.help` but no test confirms they work end-to-end.
- **Session changeset round-trip** has a smoke (we verified at Stage 6) but it's not in CI.
- **vtab fetch_batch perf claim** (~5-10× faster than per-row) has a one-time measurement in PLAN-perf-rollout.md but no regression bench. **Wire `tooling/bench.py` into CI as a "fail-if-slower-than-X" gate.**
- **Extension smoke harness exists** (`tooling/smoke.py`) but is opt-in via `make ext-ship`. Not run in CI. **Wire it in for the 30 fastest extensions as a sanity gate.**

---

## Recommended sequence

1. **Week 1 ergonomics polish (1-2 days):**
   - clap migration (#1)
   - `scripts/install-wasi-sdk.sh` + `scripts/build-all.sh` (#7.1, #7.3)
   - Error message prefix pass (#5)
   - Stale `PLAN-benchmarks.md` ref in tooling/bench.py

2. **Week 2 size + perf (2-3 days):**
   - Pre-size the execute_multi vecs + HashMap the named-param lookup (#3, #4.a)
   - Lazy-load archive-cli + sqlink-meta-cli (#7)
   - `wasm-opt -Os` in the release pipeline

3. **Week 3 refactor (3-5 days, holds release):**
   - Split host/src/lib.rs (#2)
   - Move `embed-*` cli features behind a tooling sidecar (#4)
   - Comprehensive smoke + bench in CI (#8)

Items 1-2 are pre-release polish. Item 3 is post-release maintenance — does NOT hold a v0.1.0 cut.
