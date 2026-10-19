//! 単語一覧ページ(辞書順・ページネーション付き)
//!
//! GET /words?room=<room_id>&page=<n> で選択したルームの全単語を辞書順に表示する。
//! room 未指定の場合はルーム選択ページを表示する。

use axum::{extract::Query, extract::State, response::Html};
use serde::Deserialize;

use super::search::escape_html;
use super::{AppState, SHARED_PAGE_HEAD};

/// 1ページあたりの単語数
pub const PAGE_SIZE: usize = 100;

#[derive(Deserialize, Default)]
pub struct WordsParams {
    pub room: Option<String>,
    pub page: Option<u32>,
}

/// GET /words
pub async fn page(
    State(state): State<AppState>,
    Query(params): Query<WordsParams>,
) -> Html<String> {
    let requested_room = params.room.as_deref().and_then(|s| s.parse::<u64>().ok());
    let rooms = state.manager.room_display().await;

    let mut html = SHARED_PAGE_HEAD.to_string();
    html.push_str("<h1>単語一覧</h1>\n");

    // ルームの選択(リンク一覧)
    let links = rooms
        .iter()
        .map(|(id, name)| {
            let selected = requested_room.is_some_and(|r| r == *id);
            let mark = if selected { " (表示中)" } else { "" };
            format!(
                "<li><a href=\"/words?room={id}\">{}</a>{mark}</li>\n",
                escape_html(name)
            )
        })
        .collect::<String>();

    if rooms.is_empty() {
        html.push_str("<p>登録されているルームがありません。</p>");
        html.push_str("</body></html>");
        return Html(html);
    }

    html.push_str(&format!("<h2>ルームを選択</h2><ul>{links}</ul>\n"));

    // roomが指定されていない/存在しない場合は選択ページのみ
    let Some(room_id) = requested_room else {
        html.push_str("<p><a href=\"/search\">辞書検索へ戻る</a></p>");
        html.push_str("</body></html>");
        return Html(html);
    };
    let Some((_, room_name)) = rooms.iter().find(|(id, _)| *id == room_id) else {
        html.push_str("<p>そのルームは存在しません。</p>");
        html.push_str("<p><a href=\"/search\">辞書検索へ戻る</a></p>");
        html.push_str("</body></html>");
        return Html(html);
    };

    // 全単語を辞書順で表示
    let room_arc = state.manager.get_or_new_room(room_id).await;
    let room = room_arc.read().await;
    let words = room.words.words();
    let total = words.len();
    let total_pages = total.div_ceil(PAGE_SIZE).max(1);
    let page = params
        .page
        .map(|p| p as usize)
        .unwrap_or(1)
        .clamp(1, total_pages);
    let start = (page - 1) * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(total);
    let slice = &words[start..end];

    html.push_str(&format!(
        "<h2>{} <small>({room_id}) 全{total}語</small></h2>\n",
        escape_html(room_name)
    ));

    if total == 0 {
        html.push_str("<p>このルームにはまだ単語がありません。</p>");
    } else {
        html.push_str(&format!(
            "<p>{total}語中 {start}〜{end}語目 / {page}ページ目(全{total_pages}ページ)</p>\n"
        ));
        html.push_str("<ul class=\"words\">\n");
        for word in slice {
            html.push_str(&format!("<li>{}</li>\n", escape_html(word)));
        }
        html.push_str("</ul>\n");

        // ページ移動リンク
        let mut nav = String::from("<p class=\"pagination\">");
        if page > 1 {
            nav.push_str(&format!(
                "<a href=\"/words?room={room_id}&page={}\">前のページ</a> ",
                page - 1
            ));
        }
        if page < total_pages {
            nav.push_str(&format!(
                " <a href=\"/words?room={room_id}&page={}\">次のページ</a>",
                page + 1
            ));
        }
        nav.push_str("</p>");
        html.push_str(&nav);
    }

    html.push_str("<p><a href=\"/search\">辞書検索へ戻る</a></p>");
    html.push_str("</body></html>");
    Html(html)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::RoomManager;
    use crate::web::{AppState, WebConfig};
    use crate::words::WordStore;

    async fn state_with_rooms() -> (AppState, RoomManager) {
        let manager = RoomManager::new();
        manager
            .set_room_name(1000, "TestServer/general".to_string())
            .await;
        {
            let room = manager.get_or_new_room(1000).await;
            let mut r = room.write().await;
            r.words = WordStore::default();
            for c in b'a'..=b'z' {
                // 単語をPAGE_SIZE以上(104語)用意
                for n in 0..4 {
                    r.words.add_word(&format!("{}{}", c as char, n));
                }
            }
        }
        // 空のルーム
        manager
            .set_room_name(2000, "EmptyServer/silent".to_string())
            .await;

        let state = AppState::new(
            WebConfig {
                bind: "0.0.0.0:18080".to_string(),
                public_url: "https://dict.test".to_string(),
                password: "pw".to_string(),
            },
            manager.clone(),
            "/tmp/shiritori_test_words/data.json".to_string(),
        );
        (state, manager)
    }

    /// page()を呼んでHTML本文だけ取るテストヘルパー
    async fn html_for(state: &AppState, room: Option<String>, page_num: Option<u32>) -> String {
        page(
            State(state.clone()),
            Query(WordsParams {
                room,
                page: page_num,
            }),
        )
        .await
        .0
    }

    #[tokio::test]
    async fn words_page_shows_first_page_sorted() {
        let (state, _) = state_with_rooms().await;
        let html = html_for(&state, Some("1000".to_string()), Some(1)).await;

        assert!(html.contains("TestServer/general <small>(1000) 全104語</small>"));
        assert!(html.contains("104語中 0〜100語目 / 1ページ目(全2ページ)"));
        assert!(html.contains("<li>a0</li>"));
        assert!(html.contains("<li>a3</li>"));
        // 辞書順: 先頭ページは a〜 系のみで z系は含まない
        assert!(!html.contains("<li>z0</li>"));
        assert!(html.contains("次のページ"));
        assert!(!html.contains("前のページ"));
    }

    #[tokio::test]
    async fn words_page_second_page_and_last() {
        let (state, _) = state_with_rooms().await;
        let html = html_for(&state, Some("1000".to_string()), Some(2)).await;
        assert!(html.contains("100〜104語目"));
        assert!(html.contains("<li>z3</li>"));
        assert!(html.contains("前のページ"));
        assert!(!html.contains("次のページ"));

        // 範囲外ページはクランプされる
        let html = html_for(&state, Some("1000".to_string()), Some(99)).await;
        assert!(html.contains("<li>z3</li>"));
    }

    #[tokio::test]
    async fn words_page_empty_room_and_room_select() {
        let (state, _) = state_with_rooms().await;

        // room指定なし → ルーム選択のみ(単語の表は出ない)
        let html = html_for(&state, None, None).await;
        assert!(html.contains("ルームを選択"));
        assert!(html.contains("TestServer/general"));
        assert!(html.contains("EmptyServer/silent"));

        // 空ルーム → 導線のみ
        let html = html_for(&state, Some("2000".to_string()), None).await;
        assert!(html.contains("まだ単語がありません"));

        // 存在しないルーム
        let html = html_for(&state, Some("9999".to_string()), None).await;
        assert!(html.contains("そのルームは存在しません"));
    }
}
