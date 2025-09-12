# =========================
# 1. Build stage
# =========================
FROM rust:1.89.0-bullseye as builder

WORKDIR /usr/src/app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY .env ./

RUN cargo fetch
RUN cargo build --release

# =========================
# 2. Runtime stage
# =========================
FROM debian:bullseye-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /usr/src/app/target/release/shiritori_v3 /app/shiritori_v3
COPY --from=builder /usr/src/app/.env /app/.env

# データディレクトリ作成
RUN mkdir -p /data/word /data

VOLUME ["/data/word"]

# 環境変数は .env から読み込むので不要
# ENV DISCORD_BOT_TOKEN=""
# ENV OPENROUTER_API_KEY=""
# ENV ROOMS_PATH="/data/rooms.json"
# ENV WORDS_PATH="/data/word"

ENTRYPOINT ["/bin/sh", "-c", "exec /app/shiritori_v3"]
