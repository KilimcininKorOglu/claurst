# Multi-stage: the toolchain never ships in the runtime image.
FROM rust:1.85-bookworm AS build

WORKDIR /build

# Dependencies first, so a source-only change reuses the cached layer.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src ./src
# The web client is compiled into the binary, so it has to be present here.
COPY static ./static
# Touch so cargo does not reuse the stub's timestamp and skip the real build.
RUN touch src/main.rs && cargo build --release --locked

FROM debian:bookworm-slim AS runtime

# ca-certificates is not needed today (the relay makes no outbound TLS calls),
# so the runtime stays at the base image plus the binary.
RUN useradd --system --create-home --uid 10001 relay
USER relay
WORKDIR /home/relay

COPY --from=build /build/target/release/claurst-relay /usr/local/bin/claurst-relay

# RELAY_TOKEN has no default on purpose: the relay refuses to start without one,
# and a default would be a published shared secret.
ENV RELAY_BIND=0.0.0.0:8350

EXPOSE 8350

# No shell form, so the process is PID 1 and receives SIGTERM directly for the
# graceful shutdown path.
ENTRYPOINT ["/usr/local/bin/claurst-relay"]
