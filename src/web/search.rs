//! 辞書検索ページ
//!
//! GET / と GET /search?q=...&room=...&mode=... で検索フォーム+結果を表示する。

use axum::{extract::Query, extract::State, response::Html};
use serde::Deserialize;

use crate::data::RoomManager;
use crate::words::{SearchMode, WordStore};

use super::{AppState, SHARED_PAGE_HEAD};

/// 検索モードの種類(あいまい+部分一致の複合を含む)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Fuzzy,
    Exact,
    Prefix,
    Suffix,
    Substring,
    FuzzyAndSubstring,
}

impl SearchKind {
    pub fn from_arg(value: Option<&str>) -> Self {
        match value {
            Some("exact") => Self::Exact,
            Some("prefix") => Self::Prefix,
            Some("suffix") => Self::Suffix,
            Some("substring") => Self::Substring,
            Some("fuzzy") => Self::Fuzzy,
            _ => Self::FuzzyAndSubstring,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fuzzy => "fuzzy",
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::Suffix => "suffix",
            Self::Substring => "substring",
            Self::FuzzyAndSubstring => "fuzzy_and_substring",
        }
    }

    pub fn as_label(&self) -> &'static str {
        match self {
            Self::Fuzzy => "あいまい",
            Self::Exact => "完全一致",
            Self::Prefix => "前方一致",
            Self::Suffix => "後方一致",
            Self::Substring => "部分一致",
            Self::FuzzyAndSubstring => "あいまい+部分一致",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::FuzzyAndSubstring,
            Self::Fuzzy,
            Self::Exact,
            Self::Prefix,
            Self::Suffix,
            Self::Substring,
        ]
    }

    /// WordStoreに検索を実行する
    pub fn search(&self, words: &WordStore, query: &str) -> Vec<String> {
        match self {
            Self::Fuzzy => {
                let dist = WordStore::normalize(query).chars().count() / 4;
                words.search_with_dist(query, SearchMode::Fuzzy, dist)
            }
            Self::Exact => words.search(query, SearchMode::Exact),
            Self::Prefix => words.search(query, SearchMode::Prefix),
            Self::Suffix => words.search(query, SearchMode::Suffix),
            Self::Substring => words.search(query, SearchMode::Substring),
            Self::FuzzyAndSubstring => {
                let dist = WordStore::normalize(query).chars().count() / 4;
                let mut results = words.search_with_dist(query, SearchMode::Fuzzy, dist);
                results.extend(words.search(query, SearchMode::Substring));
                results.sort();
                results.dedup();
                results
            }
        }
    }
}

#[derive(Deserialize, Default)]
pub struct SearchParams {
    pub q: Option<String>,
    pub room: Option<String>,
    pub mode: Option<String>,
}

/// GET / と GET /search 共通ハンドラ
pub async fn page(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Html<String> {
    let room_id = params.room.as_deref().and_then(|s| s.parse::<u64>().ok());
    let name = SearchKind::from_arg(params.mode.as_deref());
    let query = params.q.clone().unwrap_or_default();

    let (room_options, results) = search_rooms(&state.manager, room_id, &query, name).await;

    Html(render_search_page(
        &query,
        room_id,
        name,
        &room_options,
        &results,
    ))
}

/// 検索を実行する
///
/// room_id が None の場合は全ルームを対象にする。
/// 戻り値: (ルーム選択肢, (対象ルーム, 検索結果)のリスト)
pub(crate) async fn search_rooms(
    manager: &RoomManager,
    room_id: Option<u64>,
    query: &str,
    mode: SearchKind,
) -> (Vec<u64>, Vec<(u64, Vec<String>)>) {
    let all_room_ids = manager.room_ids().await;

    let targets: Vec<u64> = match room_id {
        Some(id) if all_room_ids.contains(&id) => vec![id],
        _ => all_room_ids.clone(),
    };

    let mut results = Vec::new();
    for room_id in targets {
        let room_arc = manager.get_or_new_room(room_id).await;
        let room = room_arc.read().await;
        let hits = if query.trim().is_empty() {
            vec![]
        } else {
            mode.search(&room.words, query)
        };
        results.push((room_id, hits));
    }

    (all_room_ids, results)
}

/// HTMLエスケープ(簡易)
pub(crate) fn escape_html(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            _ => c.to_string(),
        })
        .collect()
}

/// 検索ページHTML
pub(crate) fn render_search_page(
    query: &str,
    selected_room: Option<u64>,
    mode: SearchKind,
    room_options: &[u64],
    results: &[(u64, Vec<String>)],
) -> String {
    let mut html = SHARED_PAGE_HEAD.to_string();

    html.push_str("<h1>しりとり辞書</h1>\n");
    html.push_str(&format!(
        r#"<form method="get" action="/search">
<label>単語 <input type="text" name="q" value="{}" placeholder="検索する単語" autofocus></label>
<label>モード <select name="mode">{}</select></label>
<label>ルーム <select name="room"><option value="">全ルーム</option>{}</select></label>
<button type="submit">検索</button>
</form>"#,
        escape_html(query),
        SearchKind::all()
            .iter()
            .map(|k| {
                let selected = if *k == mode { " selected" } else { "" };
                format!(
                    r#"<option value="{}"{selected}>{}</option>"#,
                    k.as_str(),
                    k.as_label()
                )
            })
            .collect::<String>(),
        room_options
            .iter()
            .map(|id| {
                let selected = selected_room.is_some_and(|r| r == *id);
                let mark = if selected { " selected" } else { "" };
                format!(r#"<option value="{id}"{mark}>{id}</option>"#)
            })
            .collect::<String>(),
    ));

    // 結果
    for (room_id, hits) in results {
        html.push_str(&format!(
            "<h2>ルーム {} <small>({}件)</small></h2>\n",
            room_id,
            hits.len()
        ));
        if query.trim().is_empty() {
            html.push_str("<p>検索する単語を入力してください。</p>");
            continue;
        }
        if hits.is_empty() {
            html.push_str(&format!(
                "<p>「{}」に一致する単語は見つかりませんでした。</p>",
                escape_html(query)
            ));
            continue;
        }
        html.push_str("<ul class=\"words\">\n");
        for word in hits {
            html.push_str(&format!("<li>{}</li>\n", escape_html(word)));
        }
        html.push_str("</ul>\n");
    }

    html.push_str("</body></html>");
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_kind_parses_and_defaults() {
        assert_eq!(SearchKind::from_arg(Some("exact")), SearchKind::Exact);
        assert_eq!(SearchKind::from_arg(Some("prefix")), SearchKind::Prefix);
        assert_eq!(SearchKind::from_arg(Some("suffix")), SearchKind::Suffix);
        assert_eq!(
            SearchKind::from_arg(Some("substring")),
            SearchKind::Substring
        );
        assert_eq!(SearchKind::from_arg(Some("fuzzy")), SearchKind::Fuzzy);
        // 不明値/未指定 → 既定(複合)
        assert_eq!(SearchKind::from_arg(None), SearchKind::FuzzyAndSubstring);
        assert_eq!(
            SearchKind::from_arg(Some("garbage")),
            SearchKind::FuzzyAndSubstring
        );
    }

    fn store_with_words(words: &[&str]) -> WordStore {
        let mut s = WordStore::new();
        s.add_words(words.iter().map(|s| s.to_string()));
        s
    }

    #[test]
    fn search_all_kinds_hit_and_miss() {
        let words = store_with_words(&["apple", "apply", "grape"]);

        // 各モードでヒット
        assert!(!SearchKind::Exact.search(&words, "apple").is_empty());
        assert!(!SearchKind::Prefix.search(&words, "app").is_empty());
        assert!(!SearchKind::Suffix.search(&words, "ple").is_empty());
        assert!(!SearchKind::Substring.search(&words, "rap").is_empty());
        assert!(!SearchKind::Fuzzy.search(&words, "aple").is_empty());
        assert!(
            !SearchKind::FuzzyAndSubstring
                .search(&words, "apple")
                .is_empty()
        );

        // ミス
        assert!(SearchKind::Exact.search(&words, "banana").is_empty());
        assert!(
            SearchKind::FuzzyAndSubstring
                .search(&words, "zzzzzz")
                .is_empty()
        );
    }

    async fn manager_with() -> RoomManager {
        let manager = RoomManager::new();
        // 手動で1ルーム作成
        manager.get_or_new_room(1000).await;
        manager
    }

    #[tokio::test]
    async fn search_rooms_hits_all_or_single() {
        let manager = manager_with().await;
        {
            let room = manager.get_or_new_room(1000).await;
            let mut r = room.write().await;
            r.words.add_word("apple");
            r.words.add_word("grape");
        }

        let (options, results) = search_rooms(&manager, None, "app", SearchKind::Substring).await;
        assert!(options.contains(&1000));
        assert_eq!(results.len(), 1);
        assert!(results[0].1.contains(&"apple".to_string()));
        assert!(!results[0].1.contains(&"grape".to_string()));

        // 特定ルーム指定(存在する場合)
        let (_, single) = search_rooms(&manager, Some(1000), "app", SearchKind::Substring).await;
        assert_eq!(single.len(), 1);

        // 存在しないルーム指定 → 全ルームへフォールバック
        let (_, all) = search_rooms(&manager, Some(999), "app", SearchKind::Substring).await;
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn search_page_html_renders_results() {
        let manager = manager_with().await;
        {
            let room = manager.get_or_new_room(1000).await;
            room.write().await.words.add_word("apple");
        }

        let (options, results) = search_rooms(&manager, None, "app", SearchKind::Substring).await;
        let html = render_search_page("app", None, SearchKind::Substring, &options, &results);

        // フォームと結果
        assert!(html.contains("<form"));
        assert!(html.contains("value=\"app\""));
        assert!(!html.contains("見つかりませんでした"));
        assert!(html.contains("<li>apple</li>"));
        assert!(html.contains("ルーム 1000"));
    }

    #[tokio::test]
    async fn search_page_html_renders_miss() {
        let manager = manager_with().await;
        let (options, results) = search_rooms(&manager, None, "zzz", SearchKind::Exact).await;
        let html = render_search_page("zzz", None, SearchKind::Exact, &options, &results);
        assert!(html.contains("見つかりませんでした"));
    }

    #[test]
    fn html_escaping() {
        let escaped = escape_html("<script>&\"'</script>");
        assert!(!escaped.contains('<'));
        assert!(escaped.contains("&lt;script&gt;"));
        assert!(escaped.contains("&amp;"));
    }

    #[test]
    fn mode_select_default_is_combined() {
        let html = render_search_page("", None, SearchKind::FuzzyAndSubstring, &[], &[]);
        assert!(html.contains("value=\"fuzzy_and_substring\""));
        let html = render_search_page("", None, SearchKind::Exact, &[], &[]);
        assert!(html.contains(r#"<option value="exact" selected>完全一致</option>"#));
    }
}
