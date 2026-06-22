# Security Policy

## Supported versions

SQLink is pre-1.0. Only the `main` branch is supported; we don't
backport fixes to earlier tags.

| Branch | Supported |
|---|---|
| `main` | ✓ |
| Tagged releases | best-effort |

## Reporting a vulnerability

Please report security issues privately, **not** via public GitHub issues:

  - Preferred: open a [GitHub Security Advisory](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
    on this repository.
  - Alternative: email the maintainers (address in repo metadata).

Please include:

  - Affected component (host / cli / specific extension / WIT contract).
  - Reproducer if you have one.
  - Whether the issue is upstream (sqlite / wasmtime / a dependency) or
    SQLink-specific.

We'll acknowledge within ~7 days and coordinate a fix + disclosure
timeline.

## Threat model

SQLink loads third-party wasm components at runtime. The relevant
trust boundaries:

  - **Host (sqlink binary) ↔ extension component.** Extensions run
    inside wasmtime with fuel + epoch metering. They reach the host
    only through the `sqlite-loader-wit` WIT interface — no direct
    syscall surface. SQL executes against a shared host-owned
    sqlite3 connection via spi.
  - **Capability policy.** Each extension declares the host
    capabilities it needs (http, filesystem, state, etc) in its
    manifest. The host enforces a capability allow-list at load
    time (see [PLAN-grants-db.md](PLAN-grants-db.md) for the trust
    model: `--trust=manifest`, `--trust=stored`, `--trust=prompt`).
  - **Filesystem.** wasm components don't get host filesystem
    access by default. The cli passes specific files via wasi
    preopens when commands like `.read FILE` or `.insert TABLE FILE`
    need them.
  - **Network.** Extensions that declare `capability::http` can
    fetch URIs through a host-mediated client; the host applies the
    extension's manifest-declared `http-policy.allowed-domains`
    list at request time.

## Out of scope

  - Bugs in SQLite itself (report upstream at https://sqlite.org).
  - Bugs in wasmtime, wasm-tools, or other Bytecode Alliance projects.
  - Denial-of-service from intentionally-malicious extensions on a
    host the operator chose to grant capabilities to. The capability
    gate is the layer that's expected to reject those at load time;
    the runtime fuel/epoch metering is a backstop, not a guarantee.

## Known sharp edges

Tracked in [PLAN-gaps.md](PLAN-gaps.md). Notable:

  - Wasm panic stack traces don't propagate cleanly to the host.
  - Extension hot-reload is not yet supported.
