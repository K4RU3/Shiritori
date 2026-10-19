//! Web機能(axum)
//!
//! - 辞書検索ページ / 管理画面(認証付き) / v3旧データ移行アップロード / ファイル出力
//!
//! 有効条件: 環境変数 `WEB_BIND`(待ち受けIP:ポート)・`WEB_PUBLIC_URL`
//! (ユーザー遷移用公開アドレス)・`WEB_PASSWORD`(管理画面パスワード)の
//! **3つがすべて設定された場合のみ**有効。いずれか未設定なら無効となり、
//! ログ1行でDiscord起動は継続する。

pub mod admin;
pub mod migration;
pub mod search;

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use crate::data::RoomManager;
use axum::{Router, http::StatusCode, routing::get};

/// Web設定
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebConfig {
    /// 待ち受けアドレス(例: 0.0.0.0:8080)
    pub bind: String,
    /// ユーザー遷移用公開アドレス(例: https://dict.example.com)
    pub public_url: String,
    /// 管理画面パスワード
    pub password: String,
}

impl WebConfig {
    /// 環境変数から取得。3つすべて揃っている場合のみ Some を返す。
    pub fn from_env() -> Option<Self> {
        Self::from_parts(
            std::env::var("WEB_BIND").ok(),
            std::env::var("WEB_PUBLIC_URL").ok(),
            std::env::var("WEB_PASSWORD").ok(),
        )
    }

    /// 部品から組み立てて判定する(from_envの検証コア)
    pub fn from_parts(
        bind: Option<String>,
        public_url: Option<String>,
        password: Option<String>,
    ) -> Option<Self> {
        let bind = bind?;
        let public_url = public_url?;
        let password = password?;

        if bind.is_empty() || public_url.is_empty() || password.is_empty() {
            return None;
        }

        Some(Self {
            bind,
            public_url,
            password,
        })
    }
}

/// 管理画面セッションストア(メモリ保持・プロセス再起動で失効)
pub type Sessions = Arc<Mutex<HashSet<String>>>;

/// Webハンドラ共有状態
#[derive(Clone)]
pub struct AppState {
    pub config: WebConfig,
    pub manager: RoomManager,
    /// data.json のパス(出力用)
    pub data_path: String,
    pub sessions: Sessions,
}

impl AppState {
    pub fn new(config: WebConfig, manager: RoomManager, data_path: String) -> Self {
        Self {
            config,
            manager,
            data_path,
            sessions: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

/// Web機能を起動する
///
/// - バインド失敗時はエラーログのみでプロセスは落ちない(Discord接続は継続)
/// - 戻り値: 実際に起動を試みた場合 Some(待ち受けアドレス)
pub async fn serve(state: AppState) -> Result<String, String> {
    let bind = state.config.bind.clone();
    let router = build_router(state);

    match tokio::net::TcpListener::bind(&bind).await {
        Ok(listener) => {
            println!("[Web] 起動: http://{bind}");
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, router).await {
                    eprintln!("[Web] サーバーエラー: {e}");
                }
            });
            Ok(bind)
        }
        Err(e) => {
            eprintln!("[Web] 待ち受け失敗({bind}): {e} — Web機能は無効のまま継続します");
            Err(e.to_string())
        }
    }
}

/// ルーター構築
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(search::page))
        .route("/search", get(search::page))
        .route("/admin/login", get(admin::login_form).post(admin::login))
        .route("/admin", get(admin::dashboard))
        .route("/admin/export", get(admin::export))
        .route("/admin/logout", get(admin::logout))
        .route("/admin/upload", get(admin::upload_form).post(admin::upload))
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn healthz() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

pub(crate) const SHARED_PAGE_HEAD: &str = r#"<!DOCTYPE html>
<html lang="ja"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>しりとり辞書</title>
<style>
body { font-family: 'Segoe UI', system-ui, sans-serif; max-width: 860px; margin: 0 auto; padding: 1rem; background: #f4f7fb; color: #222; }
input, select, button { padding: .4rem .6rem; border-radius: .3rem; border: 1px solid #99a; font-size: 1rem; }
table { border-collapse: collapse; width: 100%; }
td, th { border-bottom: 1px solid #cccb; padding: .3rem .6rem; text-align: left; }
code, pre { background: #eee; padding: .1rem .3rem; border-radius: .2rem; }
a { color: #147; }
</style></head><body>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_requires_all_parts() {
        // 未設定 → 無効
        assert!(WebConfig::from_parts(None, None, None).is_none());
        assert!(WebConfig::from_parts(Some("0.0.0.0:8080".into()), None, None).is_none());

        // 空文字が混ざる → 無効
        assert!(
            WebConfig::from_parts(
                Some("0.0.0.0:8080".into()),
                Some("https://x.test".into()),
                Some("".into()),
            )
            .is_none()
        );

        // 3つ揃う → 有効
        let cfg = WebConfig::from_parts(
            Some("0.0.0.0:8080".into()),
            Some("https://x.test".into()),
            Some("pass".into()),
        )
        .unwrap();
        assert_eq!(cfg.bind, "0.0.0.0:8080");
        assert_eq!(cfg.public_url, "https://x.test");
        assert_eq!(cfg.password, "pass");
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let (status, body) = healthz().await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }

    #[test]
    fn shared_page_head_contains_page_scaffold() {
        assert!(SHARED_PAGE_HEAD.contains("<body>"));
        assert!(SHARED_PAGE_HEAD.contains("charset"));
    }

    #[tokio::test]
    async fn serve_fails_gracefully_on_bad_bind() {
        let config = WebConfig {
            bind: "256.256.256.256:1".to_string(),
            public_url: "https://x".to_string(),
            password: "pass".to_string(),
        };
        let state = AppState::new(
            config,
            RoomManager::new(),
            "/tmp/shiritori_test_web/data.json".to_string(),
        );
        let result = serve(state).await;
        assert!(result.is_err()); // 失敗してもプロセスエラーにならない
    }
}
