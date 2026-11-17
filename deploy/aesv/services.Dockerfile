# Every Rust service aesv-macro runs, in one image. One cargo build shares the
# workspace's crates; compose picks a binary with `command: ["/app/out/<bin>"]`,
# as upstream's docker/Dockerfile.dev does. Build context: the repo root.
FROM denoland/deno:2.9.6 AS deno

FROM rust:1.94.0-trixie AS builder
# NOTE: pin the toolchain here so rustup skips the iOS, Android and musl targets
# in rust-toolchain.toml, which no service needs.
ENV RUSTUP_TOOLCHAIN=1.94.0 SQLX_OFFLINE=true CARGO_PROFILE_RELEASE_DEBUG=0
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang \
  && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
ARG BINS="authentication_service connection_gateway_service contacts_service document_storage_service document_upload_finalizer_local_worker notification_service search_processing_service agent_harness_service static_file_service unfurl_service image_proxy_service"
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,target=/usr/local/cargo/git/db,sharing=shared \
    --mount=type=cache,target=/app/target,id=aesv-release,sharing=locked \
    cargo build --release $(printf -- '--bin %s ' $BINS) \
    && mkdir -p /app/out \
    && for b in $BINS; do strip "target/release/$b" && cp "target/release/$b" /app/out/; done

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates dumb-init curl git \
  && apt-get clean && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=deno /usr/bin/deno /usr/local/bin/deno
COPY --from=builder /app/out/ /app/out/
ENV DENO_BINARY=/usr/local/bin/deno PORT=8080
EXPOSE 8080
ENTRYPOINT ["dumb-init", "--"]
