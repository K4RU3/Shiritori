# syntax=docker/dockerfile:1.4

# =========================
# 1. Build stage
# =========================
FROM rust:1.89.0-bullseye as builder

WORKDIR /usr/src/app

# Cargo.toml と src をコピー
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# 依存関係だけ先にフェッチしてキャッシュ
RUN cargo fetch

# BuildKit secret をマウントして安全にビルド
RUN --mount=type=secret,id=discord_bot_token \
    --mount=type=secret,id=openrouter_api_key \
    --mount=type=secret,id=rooms_path \
    --mount=type=secret,id=words_path \
    cargo build --release

# =========================
# 2. Runtime stage
# =========================
FROM debian:bullseye-slim

# ランタイムに必要なライブラリ
RUN apt-get update && apt-get install -y \
    ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# ビルド済みバイナリをコピー
COPY --from=builder /usr/src/app/target/release/shiritori_v3 /app/shiritori_v3

# ホストと同期するディレクトリ構造に合わせる
# ROOMS_PATH は JSON ファイル
# WORDS_PATH はディレクトリ
VOLUME ["/data/word"]

# 必須環境変数
ENV DISCORD_BOT_TOKEN=""
ENV OPENROUTER_API_KEY=""
ENV ROOMS_PATH=""
ENV WORDS_PATH=""

# EntryPoint で環境変数をチェックしてから実行
ENTRYPOINT ["/bin/sh", "-c", "\
  if [ -z \"$DISCORD_BOT_TOKEN\" ] || [ -z \"$OPENROUTER_API_KEY\" ] || \
     [ -z \"$ROOMS_PATH\" ] || [ -z \"$WORDS_PATH\" ]; then \
       echo 'ERROR: Missing required environment variable'; exit 1; \
  fi; \
  exec /app/shiritori_v3 \
"]
