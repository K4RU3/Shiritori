# =========================
# 1. Build stage
# =========================
FROM rust:1.89.0-bookworm AS builder

WORKDIR /usr/src/app

# 依存のキャッシュを効かせるため、まずマニフェストのみコピー
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && mkdir -p src/bin
RUN cargo build --release || true

# 実ソースをコピーしてビルド
COPY src ./src
RUN touch src/main.rs && cargo build --release

# =========================
# 2. Runtime stage
# =========================
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /usr/src/app/target/release/shiritori_v4 /app/shiritori_v4

# データディレクトリ(ボリュームで永続化)
RUN mkdir -p /data
VOLUME ["/data"]

# データパスは既定で /data/data.json
ENV DATA_DIR=/data

# 必須環境変数は起動時に注入する(.envをイメージに焼かない):
#   DISCORD_BOT_TOKEN: Discordボットトークン(必須)
#   DATA_DIR: データディレクトリ(省略時 /data... 上書きしないこと)
#   WEB_BIND / WEB_PUBLIC_URL / WEB_PASSWORD: Web機能(3つ揃った場合のみ有効)

# Discord Bot はシグナル(SIGTERM)を直接受け取るため exec 形式で起動
ENTRYPOINT ["/app/shiritori_v4"]
