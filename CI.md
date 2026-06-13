# CI

## Running CI locally with `act`

The repo's `.github/workflows/ci.yml` runs the host crate's
fmt + clippy + tests plus dedicated jobs for the CAS cache and
compose-protocol unit tests. While the repo is still private and
GitHub-hosted runners aren't available, the same workflows run
under `nektos/act`:

```sh
brew install act          # or follow nektos/act's install docs
act -W .github/workflows/ci.yml
```

`act` reads `.actrc` for our defaults: the `catthehacker/ubuntu:
act-latest` medium image, arm64 container architecture (matches
Apple Silicon hosts; flip to amd64 if you need parity with
GitHub's amd64 runners), and no docker-in-docker socket
forwarding.

### Running a single job

```sh
act -W .github/workflows/ci.yml -j host-checks
act -W .github/workflows/ci.yml -j cache-tests
act -W .github/workflows/ci.yml -j compose-tests
```

### Skipping the docker pull

The first run pulls ~700 MB of `catthehacker/ubuntu:act-latest`.
Cache it locally:

```sh
docker pull catthehacker/ubuntu:act-latest
```

## What's in CI today

| Job | What | Why |
|---|---|---|
| `host-checks` | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --all-features` in `host/` | Catch formatting drift; clippy keeps the lint surface clean; tests cover the dispatch + SPI + compose plumbing |
| `cache-tests` | `cargo test --lib cache` | 6 CAS unit tests (blake3 + sha-256 dual-write, lookup roundtrip, purge, etc.) |
| `compose-tests` | `cargo test --lib compose_provider` | 4 protocol unit tests (manifest, query-scalar, query rows, prepare/step/finalize) |

## What's NOT in CI yet (deliberate)

- **WASM-side builds.** Building `cli`, `fiji-hello`, and
  the extensions in `sqlite-wasm-loader` requires wasi-sdk
  (~150 MB) plus `cargo-component`. The `.github/workflows/ci.yml`
  has a commented-out `wasm-builds` job placeholder where this
  goes.

  Until that lands, the host's `fiji_function_resolves_sqlite_runtime`
  smoke test skips gracefully when `fiji_hello.wasm` is missing
  (prints "skipping: fiji_hello.wasm not built" and exits 0).

- **The C-side legacy build.** `make cli-demo-test` builds
  `sqlite-cli-demo.wasm` (the legacy C cli). It needs the
  vendored wasi-sdk plus `wit-bindgen` + `wac-tools`. Same
  argument as above — adds a chunk of toolchain setup; not yet
  the priority.

- **`cargo audit`.** Worth adding once we lock the deps; some of
  the wasmtime transitive deps have RustSec advisories that need
  explicit accept-or-fix per-version. Track separately when we
  publish.

- **Integration tests against the loader.** The loader's tests
  live in `~/git/sqlite-wasm-loader/` — a separate repo. Adding
  cross-repo CI is out of scope until both repos are public and
  can use `actions/checkout` against each other.

## Why the `.cargo/config.toml` override matters

The repo's `.cargo/config.toml` pins `build.target = "wasm32-wasip2"`
because the legacy C-side build expects it. Native cargo runs need
the override. The workflow sets `CARGO_BUILD_TARGET=""` in the job
env to clear the pin; you may also need
`--target=x86_64-unknown-linux-gnu` (or aarch64) explicitly on
some workflows. Local `cargo test --target aarch64-apple-darwin`
is the macOS equivalent.

## Going public

When the repo flips public:

1. `.actrc` stays — it's harmless config that helps `act` users.
2. The workflows file works against GitHub's hosted runners
   unchanged (we use `runs-on: ubuntu-latest` not the
   `catthehacker` image directly).
3. Add the wasm-build job (see the commented placeholder).
4. Add `cargo audit` and pin policy.
5. Wire branch protection so PRs require these jobs green
   before merge.
