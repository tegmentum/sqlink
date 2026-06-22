# Contributing to SQLink

Thanks for your interest. SQLink is SQLite + an extension ecosystem
distributed as WebAssembly Components; the README has the high-level
shape and the per-crate READMEs cover the moving parts.

## Build

First-time setup needs [wasi-sdk](https://github.com/WebAssembly/wasi-sdk/releases).
Install it once (any version 22+ should work), then point the build at it:

```bash
# Option A: install to ~/wasi-sdk (the script auto-detects)
# Option B: install anywhere, then export WASI_SDK_PATH=/path/to/wasi-sdk-XX.X

scripts/setup-cargo-config.sh
```

That script substitutes your wasi-sdk location into the per-crate
`.cargo/config.toml` files (which are gitignored — only the
`.cargo/config.toml.template` siblings are tracked). Re-run it any time
you move or upgrade wasi-sdk.

Then:

```bash
cargo build --release                                  # host (sqlink binary)
cargo build -p sqlite-cli --target wasm32-wasip2 --release   # cli wasm
wasm-tools component new \
    target/wasm32-wasip2/release/sqlite_cli.wasm \
    -o target/wasm32-wasip2/release/sqlite_cli.component.wasm
```

A complete tour smoke lives at `examples/sqlite-utils-tour.sql`; run
it through `scripts/sqlite-utils-tour.sh` after building.

## Commit messages

[Conventional Commits 1.0.0](https://conventionalcommits.org/en/v1.0.0). No
emojis. No references to AI tools or generation tooling in commit
messages or code comments. Keep the subject line ≤ 70 characters and
explain the *why* in the body.

Common types:
  - `feat:` user-visible new capability
  - `fix:` bug fix
  - `refactor:` no behavior change
  - `docs:` docs only
  - `chore:` repo plumbing / build / metadata
  - `perf:` measurable speed/size improvement
  - `test:` test-only changes

## Before opening a PR

1. `cargo build --release` and `cargo build -p sqlite-cli --target wasm32-wasip2 --release` both pass.
2. Relevant smoke runs without errors (project has small per-extension
   smoke scripts under `tooling/`; for cli changes,
   `examples/sqlite-utils-tour.sql` is a good baseline).
3. If you touched a WIT file in `sqlite-loader-wit/`, that submodule has its
   own commit + parent-repo submodule-bump.
4. If you added a new dot command or extension, the provenance database
   (`provenance/extensions.db`) regenerates via `python3 provenance/scan.py`.

## Where the design lives

  - `ARCHITECTURE.md` — top-level wiring (host ↔ cli ↔ extension components).
  - `AUTHORING-DOTCMD-COMPONENTS.md` — build a new dot-command extension.
  - `AUTHORING-RUN-COMPONENTS.md` — build a new runnable component.
  - `PLAN-*.md` — historical and in-flight design docs. Many are
    "shipped"-tagged; the live ones say so.
  - `tooling/cli-cheatsheet.md` — every dot command + when to use it.
  - `tooling/extension-patterns.md` — shape catalog for scalar / agg / vtab / etc.
  - `tooling/lessons-learned.md` — per-ship retrospectives.

## Scope of accepted contributions

  - **Bug fixes:** always welcome.
  - **New extensions:** follow `AUTHORING-DOTCMD-COMPONENTS.md` or the
    scalar-extension pattern in `tooling/extension-patterns.md`. Check
    `PLAN-gaps.md` first — there's a curated list of what's wanted.
  - **CLI dot commands:** new commands ship as wasm extensions, not
    cli source. See `AUTHORING-DOTCMD-COMPONENTS.md`.
  - **WIT changes** to `sqlite-loader-wit/`: please discuss first via an
    issue — the WIT contract is the wide-API surface every extension
    depends on; breaking changes are coordinated carefully.
  - **Architectural rewrites:** open an issue first to align on the plan.

## License

By contributing, you agree your contributions are licensed under MIT.
See `LICENSE`.
