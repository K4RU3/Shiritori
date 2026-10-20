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

use crate::words::WordStore;

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

/// POST /admin/room/(add|delete) のフォーム
#[derive(Deserialize)]
pub struct RoomForm {
    pub room_id: String,
    #[serde(default)]
    pub name: String,
}

/// POST /admin/word/(add|delete) のフォーム
#[derive(Deserialize)]
pub struct WordForm {
    pub room_id: String,
    #[serde(default)]
    pub words: String,
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

    // 状態: ルーム集計(表示名付き)
    let room_display = state.manager.room_display().await;
    let mut total_words = 0usize;
    let mut table = String::from("<table><tr><th>ルーム</th><th>単語数</th><th>状態</th></tr>");
    for (id, name) in &room_display {
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
            "<tr><td>{} <small>({id})</small></td><td>{words}</td><td>{status}<br><small>順番待ち {queue_len}人</small></td></tr>",
            escape_html_basic(name)
        ));
    }
    table.push_str("</table>");

    html.push_str(&format!(
        "<h2>状態</h2><p>ルーム数: {} / 合計単語数: {total_words}</p>{table}",
        room_display.len()
    ));

    html.push_str(
        r#"<h2>操作</h2><ul>
<li><a href="/admin/export">現在のデータをJSONでダウンロード(/admin/export)</a></li>
<li><a href="/admin/import">新形式(v4)データのインポート(/admin/import)</a></li>
<li><a href="/admin/upload">旧形式(v3)データのインポート(/admin/upload)</a></li>
<li><a href="/admin/logout">ログアウト</a></li>
</ul>"#,
    );

    // 単語・ルームの管理フォーム
    html.push_str("<h2>単語・ルーム管理</h2>\n");
    html.push_str(
        r#"<h3>ルーム追加</h3><form method="post" action="/admin/room/add">
<label>チャンネルID <input type="number" name="room_id" required></label>
<label>表示名(任意・サーバー名/チャンネル名) <input type="text" name="name"></label>
<button type="submit">追加</button>
</form>"#,
    );
    html.push_str(&format!(
        r#"<h3>ルーム削除</h3><form method="post" action="/admin/room/delete">
<label>ルーム <select name="room_id" required>{}</select></label>
<button type="submit">削除</button>
</form>"#,
        room_options_html(&room_display, false),
    ));
    html.push_str(&format!(
        r#"<h3>単語追加</h3><form method="post" action="/admin/word/add">
<label>ルーム <select name="room_id" required>{}</select></label>
<label>単語(カンマ区切り可) <input type="text" name="words" required></label>
<button type="submit">追加</button>
</form>"#,
        room_options_html(&room_display, false),
    ));
    html.push_str(&format!(
        r#"<h3>単語削除</h3><form method="post" action="/admin/word/delete">
<label>ルーム <select name="room_id" required>{}</select></label>
<label>単語(カンマ区切り可・評価済み完全一致) <input type="text" name="words" required></label>
<button type="submit">削除</button>
</form>"#,
        room_options_html(&room_display, false),
    ));

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

// ===== 単語・ルーム管理 =====

/// 加入受付用の英単語(フレーズ可)の正規表現
///
/// 半角スペース1個を挟んだ複数トークン(例: "donkey kong")も1語として受理する。
/// 登録時は WordStore::normalize により trim+連続空白圧縮が行われる。
static WORD_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z][a-zA-Z\-]*(?:\s+[a-zA-Z][a-zA-Z\-]*)*$").unwrap()
});

/// 入力をカンマ/改行区切りで英単語(フレーズ可)を抽出・正規化(lowercase+空白圧縮)する
///
/// フレーズ内のスペースは保持されるため、区切りには空白ではなくカンマ/改行を使う。
fn parse_word_list(text: &str) -> Vec<String> {
    text.split(['\n', ','])
        .map(str::trim)
        .filter(|w| WORD_RE.is_match(w))
        .map(WordStore::normalize)
        .filter(|w| !w.is_empty())
        .collect()
}

/// ルーム選択HTML(表示名)
fn room_options_html(rooms: &[(u64, String)], default_all: bool) -> String {
    let mut html = if default_all {
        r#"<option value="">全ルーム</option>"#.to_string()
    } else {
        String::new()
    };
    for (id, name) in rooms {
        html.push_str(&format!(
            r#"<option value="{id}">{}</option>"#,
            escape_html_basic(name)
        ));
    }
    html
}

/// 管理操作の結果ページ
fn management_result(title: &str, messages: &[String]) -> Response {
    let mut html = SHARED_PAGE_HEAD.to_string();
    html.push_str(&format!("<h1>{}</h1>\n<ul>", escape_html_basic(title)));
    for msg in messages {
        html.push_str(&format!("<li>{}</li>", escape_html_basic(msg)));
    }
    html.push_str("</ul><p><a href=\"/admin\">管理画面へ戻る</a></p>");
    html.push_str("</body></html>");
    Html(html).into_response()
}

/// 変更を即時保存する
async fn save_now(state: &AppState) -> Option<String> {
    state
        .manager
        .save_file(&state.data_path)
        .await
        .err()
        .map(|e| format!("保存に失敗しました: {e}"))
}

/// POST /admin/room/add : ルーム追加(名前は任意)
pub async fn room_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    form: Form<RoomForm>,
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

    let mut messages = Vec::new();
    match form.room_id.trim().parse::<u64>() {
        Ok(room_id) => {
            let existed = state.manager.has_room(room_id).await;
            state.manager.get_or_new_room(room_id).await;
            let name = form.name.trim().to_string();
            if !name.is_empty() {
                state.manager.set_room_name(room_id, name).await;
            }
            messages.push(if existed {
                format!("ルーム {room_id} は既に存在します(既存データは保持)。")
            } else {
                format!("ルーム {room_id} を追加しました。")
            });
            if let Some(err) = save_now(&state).await {
                messages.push(err);
            }
        }
        Err(_) => messages.push(
            "room_id が正しくありません(数字のDiscordチャンネルIDで指定してください)。".to_string(),
        ),
    }

    management_result("ルーム追加", &messages)
}

/// POST /admin/room/delete : ルーム削除
pub async fn room_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    form: Form<RoomForm>,
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

    let mut messages = Vec::new();
    match form.room_id.trim().parse::<u64>() {
        Ok(room_id) => {
            if state.manager.delete_room(room_id).await {
                messages.push(format!("ルーム {room_id} を削除しました。"));
                if let Some(err) = save_now(&state).await {
                    messages.push(err);
                }
            } else {
                messages.push(format!("ルーム {room_id} は存在しません。"));
            }
        }
        Err(_) => messages.push("room_id が正しくありません。".to_string()),
    }

    management_result("ルーム削除", &messages)
}

/// POST /admin/word/add : 単語追加(ルームは既存のみ)
pub async fn word_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    form: Form<WordForm>,
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

    let mut messages = Vec::new();
    match form.room_id.trim().parse::<u64>() {
        Ok(room_id) if state.manager.has_room(room_id).await => {
            let words = parse_word_list(&form.words);
            let room_arc = state.manager.get_or_new_room(room_id).await;
            let mut room = room_arc.write().await;
            let added = room.words.add_words(&words);
            drop(room);

            if added.is_empty() {
                messages.push("追加された単語はありません(不正な形式または登録済み)。".to_string());
            } else {
                let list: Vec<String> = added.iter().map(|w| escape_html_basic(w)).collect();
                messages.push(format!(
                    "{}語を追加しました: {}",
                    added.len(),
                    list.join(", ")
                ));
            }
            if let Some(err) = save_now(&state).await {
                messages.push(err);
            }
        }
        Ok(room_id) => messages.push(format!(
            "ルーム {room_id} は存在しません(先にルームを追加してください)。"
        )),
        Err(_) => messages.push("room_id が正しくありません。".to_string()),
    }

    management_result("単語追加", &messages)
}

/// POST /admin/word/delete : 単語削除(評価済み完全一致)
pub async fn word_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    form: Form<WordForm>,
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

    let mut messages = Vec::new();
    match form.room_id.trim().parse::<u64>() {
        Ok(room_id) if state.manager.has_room(room_id).await => {
            let words = parse_word_list(&form.words);
            if words.is_empty() {
                messages.push("削除する単語が指定されていません。".to_string());
            } else {
                let room_arc = state.manager.get_or_new_room(room_id).await;
                let mut room = room_arc.write().await;
                let mut removed = Vec::new();
                let mut not_found = Vec::new();
                for word in &words {
                    if room.words.remove_word(word) {
                        removed.push(escape_html_basic(word));
                    } else {
                        not_found.push(escape_html_basic(word));
                    }
                }
                drop(room);

                if !removed.is_empty() {
                    messages.push(format!("削除した単語: {}", removed.join(", ")));
                }
                if !not_found.is_empty() {
                    messages.push(format!(
                        "辞書に無く削除できなかった単語: {}",
                        not_found.join(", ")
                    ));
                }
                if let Some(err) = save_now(&state).await {
                    messages.push(err);
                }
            }
        }
        Ok(room_id) => messages.push(format!("ルーム {room_id} は存在しません。")),
        Err(_) => messages.push("room_id が正しくありません。".to_string()),
    }

    management_result("単語削除", &messages)
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

    // ルーム名が未確定(v3由来はID表示)なのでBotに名前の再取得を促す
    if summary.rooms_created > 0 || summary.rooms_updated > 0 {
        state.request_name_refresh();
    }

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

/// POST /admin/import : v4形式(data.json)の全量インポート
///
/// - 受け取ったJSONを v4 形式として検証(format不一致/未来versionは拒否)
/// - 適用前に現状をバックアップ保存(.bakに退避)し、全ルームを置き換え
/// - 成功後はディスクへ即時保存
pub async fn import_v4_post(
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

    let mut json = None;
    let mut errors: Vec<String> = Vec::new();
    let mut mp = multipart;
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(err) => {
                errors.push(format!("アップロード読み取りエラー: {err}"));
                break;
            }
        };
        if field.name() == Some("data") {
            match field.bytes().await {
                Ok(bytes) => json = Some(String::from_utf8_lossy(&bytes).into_owned()),
                Err(err) => errors.push(format!("data.json 読み込みエラー: {err}")),
            }
        }
    }

    let result = match json {
        Some(json) => apply_v4_import(&state.manager, &state.data_path, &json).await,
        None => {
            errors.push("data.json がアップロードされていません。".to_string());
            Err(errors)
        }
    };

    // v4インポート成功時も、名前のないルームが含まれる可能性があるため再取得を促す
    if result.is_ok() {
        state.request_name_refresh();
    }

    let mut html = SHARED_PAGE_HEAD.to_string();
    match result {
        Ok(text) => {
            html.push_str("<h1>インポート結果</h1>\n<p>");
            html.push_str(&escape_html_basic(&text));
            html.push_str("</p>");
        }
        Err(errors) => {
            html.push_str("<h1>インポート失敗</h1>\n<ul>");
            for e in &errors {
                html.push_str(&format!(
                    "<li style=\"color:red\">{}</li>",
                    escape_html_basic(e)
                ));
            }
            html.push_str("</ul>");
            html.push_str(
                "<p>既存データは変更されていません(検証失敗時は置き換え前に中断しています)。</p>",
            );
        }
    }
    html.push_str(r#"<p><a href="/admin">管理画面へ戻る</a> / <a href="/admin/import">もう一度インポート</a></p>"#);
    html.push_str("</body></html>");
    Html(html).into_response()
}

/// GET /admin/import : v4形式(data.json)インポートフォーム
pub async fn import_v4_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if require_session(&state, &headers).is_err() {
        return Redirect::to("/admin/login").into_response();
    }

    let mut html = SHARED_PAGE_HEAD.to_string();
    html.push_str("<h1>v4データのインポート</h1>\n");
    html.push_str(
        "<p>新形式(shiritori-data形式の data.json)をフルでインポートします。存在データは「置き換え」が発生します。
適用前に現在の状態バックアップが自動的に保存されます。</p>\n",
    );
    html.push_str(
        r#"<form method="post" action="/admin/import" enctype="multipart/form-data">
<label>data.json: <input type="file" name="data" accept=".json"></label><br>
<button type="submit">インポート(置き換え)</button>
</form>"#,
    );
    html.push_str(
        r#"<p>※ 旧形式(v3)のデータは <a href="/admin/upload">v3インポートページ</a> を使用してください。</p>"#,
    );
    html.push_str(r#"<p><a href="/admin">管理画面へ戻る</a></p>"#);
    html.push_str("</body></html>");
    Html(html).into_response()
}

/// v4形式data.jsonの検証と全量インポート。
///
/// 戻り値: Ok=要約 / Err=メッセージ一覧(既存データは無変更)
pub async fn apply_v4_import(
    manager: &crate::data::RoomManager,
    data_path: &str,
    json: &str,
) -> Result<String, Vec<String>> {
    let data = match crate::data::SaveData::decode(json) {
        Ok(d) => d,
        Err(err) => {
            return Err(vec![format!(
                "data.json が v4 形式として正しくありません: {err}"
            )]);
        }
    };

    let mut rooms = 0usize;
    let mut words = 0usize;
    let mut votes = 0usize;
    for room_data in data.rooms.values() {
        rooms += 1;
        words += room_data.words.len();
        if room_data.vote.is_some() {
            votes += 1;
        }
    }

    // 現状をバックアップ退避(.bakに回転)
    if let Err(err) = manager.save_file(data_path).await {
        return Err(vec![format!(
            "インポート前にバックアップ退避に失敗したため中断しました: {err}"
        )]);
    }

    manager.replace_with(data).await;

    if let Err(err) = manager.save_file(data_path).await {
        return Err(vec![format!(
            "インポート後の保存に失敗しました(メモリ上は反映済み): {err}"
        )]);
    }

    Ok(format!(
        "ルーム {rooms}件 / 辞書 {words}語 / 進行中投票 {votes}件 のデータをインポートしました。"
    ))
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
    use axum::extract::Request;
    use axum::http::Method;
    use tower::ServiceExt;

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
    // ===== 単語・ルーム管理のテスト =====

    #[test]
    fn admin_parse_word_list_normalizes() {
        // フレーズ(token間の連続空白含む)も1エントリとして受理
        assert_eq!(
            parse_word_list("Apple, banana  cherry\nDOG"),
            vec!["apple", "banana cherry", "dog"]
        );
        // 記号を含むフレーズは全体が棄却される
        assert_eq!(parse_word_list("two words bad_word, cat"), vec!["cat"]);
    }

    fn authed(state: &AppState) -> HeaderMap {
        authed_headers(state)
    }

    #[tokio::test]
    async fn room_add_creates_and_sets_name() {
        use std::fs;
        fs::create_dir_all("/tmp/shiritori_test_admin/room").unwrap();
        let mut state = test_state();
        state.data_path = "/tmp/shiritori_test_admin/room/data.json".to_string();

        // 未認証は拒否
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("dict.test"));
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://dict.test"));
        let response = room_add(
            State(state.clone()),
            headers,
            Form(RoomForm {
                room_id: "123".into(),
                name: "Srv/ch".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(state.manager.room_ids().await.is_empty());

        // 認証済みで追加
        let response = room_add(
            State(state.clone()),
            authed(&state),
            Form(RoomForm {
                room_id: "123".into(),
                name: "Srv/ch".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            state.manager.room_display().await,
            vec![(123, "Srv/ch".to_string())]
        );
        // 即時保存されている
        let disk = fs::read_to_string("/tmp/shiritori_test_admin/room/data.json").unwrap();
        assert!(disk.contains("Srv/ch"));

        // 不正ID
        let response = room_add(
            State(state.clone()),
            authed(&state),
            Form(RoomForm {
                room_id: "x".into(),
                name: String::new(),
            }),
        )
        .await;
        let body = body_text(response).await;
        assert!(body.contains("room_id が正しくありません"));
    }

    #[tokio::test]
    async fn room_delete_removes_room() {
        use std::fs;
        fs::create_dir_all("/tmp/shiritori_test_admin/roomdel").unwrap();
        let mut state = test_state();
        state.data_path = "/tmp/shiritori_test_admin/roomdel/data.json".to_string();
        state.manager.get_or_new_room(555).await;

        let response = room_delete(
            State(state.clone()),
            authed(&state),
            Form(RoomForm {
                room_id: "555".into(),
                name: String::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!state.manager.has_room(555).await);
        assert!(state.manager.room_ids().await.is_empty());

        // 2回目は「存在しません」
        let response = room_delete(
            State(state.clone()),
            authed(&state),
            Form(RoomForm {
                room_id: "555".into(),
                name: String::new(),
            }),
        )
        .await;
        let body = body_text(response).await;
        assert!(body.contains("存在しません"));
    }

    #[tokio::test]
    async fn word_add_and_delete_operate_on_room_dictionary() {
        use std::fs;
        fs::create_dir_all("/tmp/shiritori_test_admin/word").unwrap();
        let mut state = test_state();
        state.data_path = "/tmp/shiritori_test_admin/word/data.json".to_string();
        state.manager.get_or_new_room(888).await;

        // 追加
        let response = word_add(
            State(state.clone()),
            authed(&state),
            Form(WordForm {
                room_id: "888".into(),
                words: "Apple, banana split, bad_word".into(),
            }),
        )
        .await;
        let body = body_text(response).await;
        assert!(body.contains("2語を追加しました"));
        let room = state.manager.get_or_new_room(888).await;
        assert!(room.read().await.words.contains("apple"));
        // フレーズも1語として登録される(空白は正規化される)
        assert!(room.read().await.words.contains("banana split"));
        assert!(!room.read().await.words.contains("bad_word"));

        // 削除(評価済み完全一致・大文字でも可)
        let response = word_delete(
            State(state.clone()),
            authed(&state),
            Form(WordForm {
                room_id: "888".into(),
                words: "APPLE, missing".into(),
            }),
        )
        .await;
        let body = body_text(response).await;
        assert!(body.contains("削除した単語: apple"));
        assert!(body.contains("削除できなかった単語: missing"));
        let room = state.manager.get_or_new_room(888).await;
        assert!(!room.read().await.words.contains("apple"));
        drop(room);

        // 保存に反映
        let disk = fs::read_to_string("/tmp/shiritori_test_admin/word/data.json").unwrap();
        assert!(disk.contains("banana split"));
        assert!(!disk.contains("apple"));

        // 存在しないルーム
        let response = word_add(
            State(state.clone()),
            authed(&state),
            Form(WordForm {
                room_id: "999".into(),
                words: "cat".into(),
            }),
        )
        .await;
        let body = body_text(response).await;
        assert!(body.contains("存在しません"));
    }
    // ===== v4形式(data.json)インポート =====

    #[tokio::test]
    async fn apply_v4_import_replaces_everything_and_backs_up() {
        use std::fs;

        let dir = "/tmp/shiritori_test_admin/v4import";
        fs::create_dir_all(dir).unwrap();
        let path = format!("{dir}/data.json");
        let _ = fs::remove_file(format!("{path}.bak"));

        let manager = crate::data::RoomManager::new();
        {
            let room = manager.get_or_new_room(1000).await;
            room.write().await.words.add_word("banana");
        }
        manager.save_file(&path).await.unwrap();

        // v4形式の入力(既存room 1000は消え、999に置き換わる)
        let json = r#"{
            "format": "shiritori-data",
            "version": 1,
            "saved_at": 1,
            "rooms": {
                "999": { "name": "Srv/ch", "user_queue": [3, 4], "words": ["apple", "GRAPE"], "vote": null }
            }
        }"#;
        let summary = apply_v4_import(&manager, &path, json).await.unwrap();
        assert!(summary.contains("ルーム 1件"));
        assert!(summary.contains("辞書 2語"));

        // 既存ルームは消え、新データに置き換わる
        assert!(!manager.has_room(1000).await);
        let room = manager.get_or_new_room(999).await;
        let r = room.read().await;
        assert!(r.words.contains("apple"));
        assert!(r.words.contains("grape")); // 正規化(lowercase)も有効
        assert_eq!(r.user_queue, vec![3, 4]);
        assert_eq!(r.name.as_deref(), Some("Srv/ch"));
        drop(r);

        // ディスクに即時保存されている
        let disk = fs::read_to_string(&path).unwrap();
        assert!(disk.contains("Srv/ch"));
        // 現状のバックアップ(.bak)が退避されている
        let bak = fs::read_to_string(format!("{path}.bak")).unwrap();
        assert!(bak.contains("banana"));
        assert!(!bak.contains("Srv/ch"));

        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn apply_v4_import_rejects_invalid_data_unchanged() {
        use std::fs;

        let dir = "/tmp/shiritori_test_admin/v4badimport";
        fs::create_dir_all(dir).unwrap();
        let path = format!("{dir}/data.json");
        let manager = crate::data::RoomManager::new();
        {
            let room = manager.get_or_new_room(1000).await;
            room.write().await.words.add_word("banana");
        }
        manager.save_file(&path).await.unwrap();

        // 破損JSON
        assert!(
            apply_v4_import(&manager, &path, "not json{{")
                .await
                .is_err()
        );
        // format不一致
        let err = apply_v4_import(
            &manager,
            &path,
            r#"{"format":"other","version":9,"saved_at":1,"rooms":{}}"#,
        )
        .await
        .unwrap_err();
        assert!(err[0].contains("v4 形式として正しくありません"));

        // 既存データ無変更
        let room = manager.get_or_new_room(1000).await;
        assert!(room.read().await.words.contains("banana"));

        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn import_v4_post_requires_session_and_parses_multipart() {
        use std::fs;

        let dir = "/tmp/shiritori_test_admin/v4post";
        fs::create_dir_all(dir).unwrap();
        let mut state = test_state();
        state.data_path = format!("{dir}/data.json");
        state.manager.get_or_new_room(1).await;

        let json = r#"{"format":"shiritori-data","version":1,"saved_at":1,"rooms":{"77":{"user_queue":[2,3],"words":["pear"]}}}"#;
        let body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"data\"; filename=\"data.json\"\r\nContent-Type: application/json\r\n\r\n{j}\r\n--{b}--\r\n",
            b = "--bOwN---dArY--".trim_start_matches("--"),
            j = json
        );

        // 未認証 → 302
        let request = Request::builder()
            .method(Method::POST)
            .uri("/admin/import")
            .header(
                "content-type",
                "multipart/form-data; boundary=bOwN---dArY--",
            )
            .body(axum::body::Body::from(body.clone()))
            .unwrap();
        let router = crate::web::build_router(state.clone());
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(state.manager.has_room(1).await);

        // 認証済み → 成功・置き換え
        let token = random_token();
        state.sessions.lock().unwrap().insert(token.clone());
        let request = Request::builder()
            .method(Method::POST)
            .uri("/admin/import")
            .header(
                "content-type",
                "multipart/form-data; boundary=bOwN---dArY--",
            )
            .header("cookie", format!("{COOKIE_NAME}={token}"))
            .body(axum::body::Body::from(body))
            .unwrap();
        let router = crate::web::build_router(state.clone());
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let text = body_text(response).await;
        assert!(text.contains("インポート結果"));
        assert!(text.contains("ルーム 1件"));

        // ルーム1は消えて77に置き換わっている
        assert!(!state.manager.has_room(1).await);
        assert!(state.manager.has_room(77).await);
        let room = state.manager.get_or_new_room(77).await;
        assert!(room.read().await.words.contains("pear"));

        let _ = fs::remove_dir_all(dir);
    }
}
