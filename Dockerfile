# Build the mergequeue backend (REST API + queue engine worker).
# The crate is a workspace member (members: backend, backend/migration), so the
# build context is the repo root and we build the `mergequeue` binary from it.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY backend backend
RUN cargo build --release --locked --bin mergequeue

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/mergequeue /usr/local/bin/mergequeue
EXPOSE 8080
# Config comes from MQ_* env vars — `docker run --env-file .env ghcr.io/lumaprism/mergequeue`.
ENTRYPOINT ["mergequeue"]
