FROM rust:1.93-bookworm AS builder

WORKDIR /src
COPY . .
# Build only the bridge binary (not --all-targets, which can cause
# duplicate crate resolution for the cdylib + rlib plugin crate).
RUN cargo build --release -p zenoh-bridge-lcm

# ---------- Test stage ----------
# Build test binaries without running them. Tests are executed
# via docker-compose command so exit codes propagate correctly.
FROM builder AS tester
RUN cargo test --workspace --no-run 2>&1
CMD ["cargo", "test", "--workspace"]

# ---------- Runtime stage ----------
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/zenoh-bridge-lcm /usr/local/bin/
COPY DEFAULT_CONFIG.json5 /etc/zenoh-bridge-lcm/conf.json5
ENTRYPOINT ["zenoh-bridge-lcm"]
