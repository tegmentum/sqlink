# Build the sqlite-wasm-httpd binary + bake the registry DB into
# a slim runtime layer. Tested for Cloud Run; works on any
# container runtime that respects $PORT.
#
# Build:
#   docker build -t extensions-site -f extensions-site/Dockerfile .
# Run:
#   docker run --rm -p 8080:8080 -e PORT=8080 extensions-site

# ----- Stage 1: compile the httpd binary -------------------------
FROM rust:1.83-bookworm AS builder

WORKDIR /src

# Copy the workspace + the httpd crate. The httpd has a sibling
# host crate dep; pull the whole workspace to avoid hand-juggling
# the dep tree.
COPY Cargo.toml Cargo.lock ./
COPY sqlite-wasm-httpd ./sqlite-wasm-httpd
COPY host ./host
COPY core ./core
COPY cli ./cli
COPY sqlite-loader-wit ./sqlite-loader-wit
COPY sqlite-cas-cache ./sqlite-cas-cache
COPY sqlite-cli-embedded ./sqlite-cli-embedded
COPY sqlite-embed ./sqlite-embed
COPY tvm-wasm ./tvm-wasm
COPY wit ./wit
COPY src ./src
COPY .cargo ./.cargo

RUN cargo build --release -p sqlite-wasm-httpd

# ----- Stage 2: build the registry DB ----------------------------
# Walks every extension's target/wasm32-wasip2/release/*.component.wasm
# and bakes it into the assets table of registry.db. The httpd's
# `blob` route kind serves /asset/<name> directly out of that table
# no external storage, no egress fees, no DNS to configure.
#
# Expects extensions to be pre-built on the host before `docker build`
# either via `make ext NAME=<name>` per extension or the workspace
# Makefile. The Docker build copies the .component.wasm artifacts
# from the host tree.
FROM python:3.12-slim AS db-builder
WORKDIR /src
COPY extensions /src/extensions
COPY extensions-site /src/extensions-site
COPY registry /src/registry
COPY provenance /src/provenance
RUN python3 /src/provenance/build_registry.py && \
    python3 /src/extensions-site/build.py

# ----- Stage 3: slim runtime layer -------------------------------
# Just the httpd binary + the registry.db.

FROM debian:bookworm-slim AS runtime

# Install ca-certificates for outbound HTTPS, even though the site
# itself only serves. Skip the recommended bloat.
RUN apt-get update && \
    apt-get install --no-install-recommends -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/sqlite-wasm-httpd /usr/local/bin/sqlite-wasm-httpd
COPY --from=db-builder /src/extensions-site/registry.db /var/lib/registry/registry.db

# Cloud Run sets $PORT  honor it. Default 8080 for local docker.
ENV PORT=8080

# Image size accounting (approx, current catalog):
#   debian:bookworm-slim base   75 MB
#   sqlite-wasm-httpd binary   21 MB
#   registry.db (200+ exts + 88 MB CAS)   90 MB
#   total                     186 MB
# Well within Cloud Run's 8 GiB image limit.

# Bind on all interfaces. The registry DB is mounted readonly-ish
# (the httpd opens it RW for SQLite's session machinery but never
# writes; container fs is ephemeral anyway).
ENTRYPOINT ["/bin/sh", "-c", "exec /usr/local/bin/sqlite-wasm-httpd --db /var/lib/registry/registry.db --bind 0.0.0.0 --port ${PORT}"]

EXPOSE 8080
