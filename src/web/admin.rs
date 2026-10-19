//! 管理画面(パスワード認証付き)
//!
//! - GET  /admin/login : パスワードフォーム
//! - POST /admin/login : 認証成功で乱数トークン発行・Cookie(httpOnly)設定・/admin へリダイレクト
//! - GET  /admin       : 状態表示(ルーム/単語数・公開URL案内・出力/アップロード/ログアウトへのリンク)
//! - GET  /admin/logout: トークン失効とCookie削除
//!
//! /admin 直下はセッショントークン(Cookie)による認証が必須。
//! 未認証は /admin/login へリダイレクトする。
//!
//! 軽量CSRF対策: POST時は Origin/Referer ヘッダのホストと Host ヘッダが
//! 一致しないリクエストを403で拒否する(presentな場合のみ判定)。

use axum::{
    extract::{Form, Multipart, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use super::{AppState, SHARED_PAGE_HEAD, migration};

/// 管理セッションCookie名
pub const COOKIE_NAME: &str = "shiritori_admin_token";

/// 乱数トークン生成(128bit)
fn random_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let a: u64 = rng.random();
    let b: u64 = rng.random();
    format!("{a:016x}{b:016x}")
}

#[derive(Deserialize)]
pub struct LoginForm {
    pub password: String,
}

/// GET /admin/login : ログインフォーム
pub async fn login_form() -> Html<String> {
    let mut html = SHARED_PAGE_HEAD.to_string();
    html.push_str("<h1>管理画面</h1>\n");
    html.push_str(
        r#"<form method="post" action="/admin/login">
<label>パスワード <input type="password" name="password" required></label>
<button type="submit">ログイン</button>
</form>"#,
    );
    html.push_str("</body></html>");
    Html(html)
}

/// POST登録Origin/Refererの軽量検証
fn is_same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return true; // Hostが取れない場合(内部テスト等)は許可
    };
    let normal = |host: &str| host.split(':').next().unwrap_or(host).trim().to_lowercase();

    for key in [header::ORIGIN, header::REFERER] {
        if let Some(value) = headers.get(key).and_then(|v| v.to_str().ok()) {
            let origin_host = value
                .rsplit("//")
                .next()
                .unwrap_or(value)
                .split('/')
                .next()
                .unwrap_or(value);
            if normal(origin_host) != normal(host) {
                return false;
            }
        }
    }
    true
}

/// POST /admin/login
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    form: Form<LoginForm>,
) -> Response {
    if !is_same_origin(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Html("不正な送信元のため拒否しました。".to_string()),
        )
            .into_response();
    }

    if form.password != state.config.password {
        return (
            StatusCode::UNAUTHORIZED,
            Html("パスワードが正しくありません。".to_string()),
        )
            .into_response();
    }

    let token = random_token();
    state
        .sessions
        .lock()
        .expect("session mutex")
        .insert(token.clone());

    let mut response = Redirect::to("/admin").into_response();
    let cookie = format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

/// Cookieから管理トークンを取り出す
pub fn session_token(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in cookie.split(';') {
        let pair = pair.trim();
        if let Some(rest) = pair.strip_prefix(COOKIE_NAME) {
            let token = rest.strip_prefix('=')?;
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// セッション認証チェック。失敗時は Reason 付きエラー
pub fn require_session(state: &AppState, headers: &HeaderMap) -> Result<(), Redirect> {
    let token = session_token(headers).unwrap_or_default();
    let valid = state
        .sessions
        .lock()
        .expect("session mutex")
        .contains(&token);
    if valid {
        Ok(())
    } else {
        Err(Redirect::to("/admin/login"))
    }
}

/// GET /admin : 管理ダッシュボード
pub async fn dashboard(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if require_session(&state, &headers).is_err() {
        return Redirect::to("/admin/login").into_response();
    }

    let mut html = SHARED_PAGE_HEAD.to_string();
    html.push_str("<h1>管理画面</h1>\n");
    html.push_str(&format!(
        "<p>Webアプリを <a href=\"/search\">辞書検索</a> でも利用できます。<br>公開URL: <a href=\"{}\">{}</a></p>\n",
        state.config.public_url, state.config.public_url
    ));

    // 状態: ルーム集計
    let room_ids = state.manager.room_ids().await;
    let mut total_words = 0usize;
    let mut table = String::from("<table><tr><th>ルームID</th><th>単語数</th><th>状態</th></tr>");
    for id in &room_ids {
        let room_arc = state.manager.get_or_new_room(*id).await;
        let room = room_arc.read().await;
        let words = room.words.len();
        total_words += words;
        let status = if let Some(vote) = &room.vote {
            format!(
                "進行中投票: {} ({}票)",
                vote.word,
                vote.good.len() + vote.bad.len()
            )
        } else {
            "待機".to_string()
        };
        let queue_len = room.user_queue.len();
        table.push_str(&format!(
            "<tr><td>{id}</td><td>{words}</td><td>{status}<br><small>順番待ち {queue_len}人</small></td></tr>"
        ));
    }
    table.push_str("</table>");

    html.push_str(&format!(
        "<h2>状態</h2><p>ルーム数: {} / 合計単語数: {total_words}</p>{table}",
        room_ids.len()
    ));

    html.push_str(
        r#"<h2>操作</h2><ul>
<li><a href="/admin/export">現在のデータをJSONでダウンロード(/admin/export)</a></li>
<li><a href="/admin/upload">旧形式(v3)データのインポート(/admin/upload)</a></li>
<li><a href="/admin/logout">ログアウト</a></li>
</ul>"#,
    );

    html.push_str("</body></html>");
    Html(html).into_response()
}

/// GET /admin/logout
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        state.sessions.lock().expect("session mutex").remove(&token);
    }

    let mut response = Redirect::to("/admin/login").into_response();
    let cookie = format!("{COOKIE_NAME}=deleted; Path=/; HttpOnly; Max-Age=0");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

/// GET /admin/export : 現在のデータを新形式JSONでダウンロード
pub async fn export(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if require_session(&state, &headers).is_err() {
        return Redirect::to("/admin/login").into_response();
    }

    let data = state.manager.snapshot().await;
    let json = serde_json::to_string_pretty(&data).unwrap_or_else(|_| "{}".to_string());

    (
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"data.json\"",
            ),
        ],
        json,
    )
        .into_response()
}

/// アップロード入力(multipartから収集したもの)
#[derive(Debug, Default)]
pub struct UploadInput {
    /// rooms.json の中身
    pub rooms_json: Option<String>,
    /// word_{id}.idx の (room_id, バイト列)
    pub idx_files: Vec<(u64, Vec<u8>)>,
    /// 収集時の警告(解析不可ファイル等)
    pub errors: Vec<String>,
}

/// multipartボディからアップロード入力を取り出す
pub async fn extract_upload(mut multipart: Multipart) -> UploadInput {
    let mut input = UploadInput::default();

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => {
                input
                    .errors
                    .push(format!("アップロード読み取りエラー: {err}"));
                break;
            }
        };

        match field.name().unwrap_or_default() {
            "rooms" => match field.text().await {
                Ok(text) => input.rooms_json = Some(text),
                Err(err) => input
                    .errors
                    .push(format!("rooms.json 読み込みエラー: {err}")),
            },
            "idx" => {
                let filename = field.file_name().unwrap_or_default().to_string();
                let room_id = migration::parse_v3_idx_filename(&filename);
                match (field.bytes().await, room_id) {
                    (Ok(bytes), Some(id)) => input.idx_files.push((id, bytes.to_vec())),
                    (Ok(_), None) => {
                        input.errors.push(format!(
                            "無視: idx以外の名称「{filename}」(word_*.idx のみ有効)"
                        ));
                    }
                    (Err(err), _) => {
                        input
                            .errors
                            .push(format!("{filename} 読み込みエラー: {err}"));
                    }
                }
            }
            _ => {}
        }
    }

    input
}

/// バックアップ退避 → 旧データの検証・マージ → マージ済みデータの保存までを実行
///
/// 戻り値: (移行要約, 警告一覧)
pub async fn apply_import(
    manager: &crate::data::RoomManager,
    data_path: &str,
    input: &UploadInput,
) -> (migration::ImportSummary, Vec<String>) {
    let mut errors = input.errors.clone();

    // 旧idxのbincodeデコード。失敗ファイルは警告として記録し残りは続行
    let mut word_lists: Vec<(u64, Vec<String>)> = Vec::new();
    for (room_id, bytes) in &input.idx_files {
        match migration::decode_v3_words(bytes) {
            Ok(words) => word_lists.push((*room_id, words)),
            Err(err) => errors.push(format!("{room_id}.idx 解析エラー: {err}")),
        }
    }

    // rooms.jsonの中身を検証(失敗なら警告へ渡して単語だけマージ)
    let mut rooms_json = input.rooms_json.clone();
    if let Some(json) = &rooms_json
        && let Err(err) = migration::decode_v3_rooms(json)
    {
        errors.push(format!("rooms.json 解析エラー: {err}"));
        rooms_json = None;
    }

    // 適用前に現在の状態をバックアップ退避(直前の状態が保存され、旧 .bak が回転)
    if let Err(err) = manager.save_file(data_path).await {
        errors.push(format!("バックアップ退避に失敗: {err}"));
    }

    let summary = match migration::import_v3(manager, rooms_json.as_deref(), &word_lists).await {
        Ok(s) => s,
        Err(err) => {
            errors.push(format!("インポート処理に失敗: {err}"));
            migration::ImportSummary::default()
        }
    };

    // マージ済みデータを永続化
    if let Err(err) = manager.save_file(data_path).await {
        errors.push(format!("マージ後の保存に失敗: {err}"));
    }

    (summary, errors)
}

/// POST /admin/upload : v3旧形式データ(rooms.json + word_*.idx)のインポート
pub async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    if require_session(&state, &headers).is_err() {
        return Redirect::to("/admin/login").into_response();
    }
    if !is_same_origin(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Html("不正な送信元のため拒否しました。".to_string()),
        )
            .into_response();
    }

    let input = extract_upload(multipart).await;
    let (summary, errors) = apply_import(&state.manager, &state.data_path, &input).await;

    // 結果ページ
    let mut html = SHARED_PAGE_HEAD.to_string();
    html.push_str("<h1>インポート結果</h1>\n<ul>");
    html.push_str(&format!("<li>新規ルーム: {}</li>", summary.rooms_created));
    html.push_str(&format!("<li>更新ルーム: {}</li>", summary.rooms_updated));
    html.push_str(&format!("<li>追加単語: {}</li>", summary.words_added));
    html.push_str(&format!("<li>復元投票: {}</li>", summary.votes_restored));
    html.push_str("</ul>");
    if !errors.is_empty() {
        html.push_str("<h2>警告</h2><ul>");
        for e in &errors {
            html.push_str(&format!("<li>{e}</li>"));
        }
        html.push_str("</ul>");
    }
    html.push_str(r#"<p><a href="/admin">管理画面へ戻る</a></p>"#);
    // /admin/upload のGETフォームへのリンクも掲載
    html.push_str(r#"<p><a href="/admin/upload">別のデータをインポート</a></p>"#);
    html.push_str("</body></html>");
    Html(html).into_response()
}

/// GET /admin/upload : アップロードフォーム
pub async fn upload_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if require_session(&state, &headers).is_err() {
        return Redirect::to("/admin/login").into_response();
    }

    let mut html = SHARED_PAGE_HEAD.to_string();
    html.push_str("<h1>v3データのインポート</h1>\n");
    html.push_str("<p>旧形式(rooms.json と word_*.idx)をzipからファイル単位でアップロードします。既存のv4データは保護されます(単語は追加マージ、キューと投票は空の場合同置き換え)。適用前に現状がバックアップ保存されます。</p>\n");
    html.push_str(
        r#"<form method="post" action="/admin/upload" enctype="multipart/form-data">
<label>rooms.json (任意): <input type="file" name="rooms" accept=".json"></label><br>
<label>word_*.idx (複数可): <input type="file" name="idx" multiple accept=".idx"></label><br>
<button type="submit">インポート</button>
</form>"#,
    );
    if let Some(err) = params.get("err") {
        html.push_str(&format!(
            "<p style=\"color:red\">警告: {}</p>",
            escape_html_basic(err)
        ));
    }
    html.push_str(r#"<p><a href="/admin">管理画面へ戻る</a></p>"#);
    html.push_str("</body></html>");
    Html(html).into_response()
}

/// HTMLエスケープ(最小限・クエリパラメータ表示用)
fn escape_html_basic(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::RoomManager;
    use crate::web::{AppState, WebConfig};

    fn test_state() -> AppState {
        AppState::new(
            WebConfig {
                bind: "0.0.0.0:18080".to_string(),
                public_url: "https://dict.test".to_string(),
                password: "secret".to_string(),
            },
            RoomManager::new(),
            "/tmp/shiritori_test_admin/data.json".to_string(),
        )
    }

    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 100_000)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[test]
    fn random_token_is_unique() {
        let t1 = random_token();
        let t2 = random_token();
        assert_eq!(t1.len(), 32);
        assert_ne!(t1, t2);
    }

    #[test]
    fn csrf_guard_rejects_cross_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("dict.test"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.test"),
        );
        assert!(!is_same_origin(&headers));

        // Origin が一致 → 許可
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("dict.test"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://dict.test"),
        );
        assert!(is_same_origin(&headers));

        // ヘッダ欠如 → 許可(api/test クライアント想定)
        assert!(is_same_origin(&HeaderMap::new()));
    }

    #[tokio::test]
    async fn login_rejects_wrong_password() {
        let state = test_state();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("dict.test"));
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://dict.test"));

        let response = login(
            State(state),
            headers,
            Form(LoginForm {
                password: "wrong".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = body_text(response).await;
        // Html述エラー文言
        assert!(body.contains("パスワードが正しくありません"));
    }

    #[tokio::test]
    async fn login_sets_session_cookie() {
        let state = test_state();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("dict.test"));
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://dict.test"));

        let response = login(
            State(state.clone()),
            headers,
            Form(LoginForm {
                password: "secret".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.starts_with(COOKIE_NAME));
        assert!(set_cookie.contains("HttpOnly"));
        let token = set_cookie
            .split(';')
            .next()
            .unwrap()
            .trim_start_matches(COOKIE_NAME)
            .trim_start_matches('=')
            .to_string();

        // セッションが保持されている
        assert!(state.sessions.lock().unwrap().contains(&token));
    }

    #[tokio::test]
    async fn dashboard_requires_authentication() {
        let state = test_state();
        let response = dashboard(State(state.clone()), HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(state.sessions.lock().unwrap().is_empty());

        // 有効トークン → ダッシュボード
        let token = random_token();
        state.sessions.lock().unwrap().insert(token.clone());
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{COOKIE_NAME}={token}")).unwrap(),
        );
        let response = dashboard(State(state.clone()), headers).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("管理画面"));
        assert!(body.contains("/admin/export"));
        assert!(body.contains("https://dict.test"));
    }

    #[tokio::test]
    async fn logout_revokes_session() {
        let state = test_state();
        let token = random_token();
        state.sessions.lock().unwrap().insert(token.clone());

        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{COOKIE_NAME}={token}")).unwrap(),
        );
        let response = logout(State(state.clone()), headers).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(!state.sessions.lock().unwrap().contains(&token));

        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.contains("Max-Age=0"));
    }

    #[test]
    fn session_token_extraction() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("shiritori_admin_token=abc123"),
        );
        assert_eq!(session_token(&headers).as_deref(), Some("abc123"));

        // 他Cookieが混在していても取り出せる
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("other=x; shiritori_admin_token=tok2; last=y"),
        );
        assert_eq!(session_token(&headers).as_deref(), Some("tok2"));

        // 欠落
        assert!(session_token(&HeaderMap::new()).is_none());
    }

    fn authed_headers(state: &AppState) -> HeaderMap {
        let token = random_token();
        state.sessions.lock().unwrap().insert(token.clone());
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{COOKIE_NAME}={token}")).unwrap(),
        );
        headers
    }

    #[tokio::test]
    async fn export_requires_session() {
        let state = test_state();
        let response = export(State(state.clone()), HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(state.sessions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn export_returns_data_json_attachment() {
        use std::fs;
        let dir = "/tmp/shiritori_test_admin/export";
        fs::create_dir_all(dir).unwrap();
        let path = format!("{dir}/data.json");

        // 既存状態を保存しておく(exportはsnapshotをそのまま出力)
        let state = test_state();
        state.manager.save_file(&path).await.unwrap();

        let response = export(State(state.clone()), authed_headers(&state)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"data.json\""
        );
        let body = body_text(response).await;
        assert!(body.contains("shiritori-data"));
        // ディスクのファイルと同一内容
        let disk = fs::read_to_string(&path).unwrap();
        assert_eq!(disk, body);
    }

    #[tokio::test]
    async fn apply_import_merges_and_backs_up() {
        use crate::web::migration;
        use std::fs;

        let dir = "/tmp/shiritori_test_admin/apply";
        fs::create_dir_all(dir).unwrap();
        let path = format!("{dir}/data.json");
        fs::remove_file(format!("{path}.bak")).ok();

        let manager = crate::data::RoomManager::new();
        // 既存v4ルーム
        {
            let room = manager.get_or_new_room(1000).await;
            let mut r = room.write().await;
            r.user_queue = vec![5, 6];
            r.words.add_word("banana");
        }
        manager.save_file(&path).await.unwrap();

        let input = UploadInput {
            rooms_json: Some(
                r#"[
                { "room_id": 1000, "user_queue": [77] },
                { "room_id": 2000, "user_queue": [9] }
            ]"#
                .to_string(),
            ),
            idx_files: vec![
                (
                    1000,
                    migration::encode_v3_index_for_tests(&["apple", "Apple", "new1"]),
                ),
                (2000, migration::encode_v3_index_for_tests(&["grape"])),
                // 壊れたidx: 警告になりスキップされる
                (3000, vec![0x00, 0x01, 0x02, 0x03]),
            ],
            errors: vec![],
        };

        let (summary, errors) = apply_import(&manager, &path, &input).await;

        // マージ内容
        let room1 = manager.get_or_new_room(1000).await;
        let r1 = room1.write().await;
        assert_eq!(r1.user_queue, vec![5, 6]); // 既存キュー保護
        assert!(r1.words.contains("apple"));
        assert!(r1.words.contains("new1"));
        drop(r1);
        let room2 = manager.get_or_new_room(2000).await;
        let r2 = room2.write().await;
        assert_eq!(r2.user_queue, vec![9]);
        assert!(r2.words.contains("grape"));
        drop(r2);

        assert_eq!(summary.rooms_created, 1); // 2000
        assert_eq!(summary.rooms_updated, 1); // 1000
        assert_eq!(summary.words_added, 3); // apple, new1, grape
        assert_eq!(errors.len(), 1); // 壊れたidxのみ

        // マージ後の保存(data.jsonが更新・.bakにマージ前の状態が退避)
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("apple"));
        let backup = fs::read_to_string(format!("{path}.bak")).unwrap();
        assert!(backup.contains("banana"));
        assert!(!backup.contains("apple"));
    }

    #[tokio::test]
    async fn apply_import_survives_broken_rooms_json() {
        use crate::web::migration;
        use std::fs;

        let dir = "/tmp/shiritori_test_admin/apply_broken";
        fs::create_dir_all(dir).unwrap();
        let path = format!("{dir}/data.json");

        let manager = crate::data::RoomManager::new();
        manager.save_file(&path).await.unwrap();

        let input = UploadInput {
            rooms_json: Some("not json at all".to_string()),
            idx_files: vec![(1000, migration::encode_v3_index_for_tests(&["pear"]))],
            errors: vec![],
        };

        let (summary, errors) = apply_import(&manager, &path, &input).await;
        assert!(errors.iter().any(|e| e.contains("rooms.json 解析エラー")));
        // 単語だけはマージされる
        let room = manager.get_or_new_room(1000).await;
        let r = room.write().await;
        assert!(r.words.contains("pear"));
        drop(r);
        assert_eq!(summary.words_added, 1);
    }
}
