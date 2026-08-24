FROM rust:1.98-slim AS builder

WORKDIR /build
COPY . .

RUN apt-get update && apt-get install -y pkg-config libssl-dev build-essential cmake clang && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --bin nab-mcp --no-default-features

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/nab-mcp /usr/local/bin/nab-mcp

ENTRYPOINT ["nab-mcp"]
