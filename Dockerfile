FROM rust:1.88-bookworm AS builder
WORKDIR /app

# Cache dependencies in a separate layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/youtube-sub-role target/release/deps/youtube_sub_role*

# Build actual source
COPY src/ src/
COPY migrations/ migrations/
COPY favicon.ico ./
RUN cargo build --release && strip target/release/youtube-sub-role

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/youtube-sub-role /usr/local/bin/
EXPOSE 8080
CMD ["youtube-sub-role"]
