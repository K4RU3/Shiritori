# Build Stage
FROM rust:1.81 as builder
WORKDIR /app
RUN cargo build --release

# Runtime Stage
FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/bot-server /usr/local/bin/bot-server
CMD []