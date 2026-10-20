//! チャンネル単位の単語辞書ストア
//!
//! - 正規化: trim + lowercase + 連続空白の圧縮(フレーズ対応)
//! - 完全一致: HashSet / 内部配列 による O(1) 判定
//! - 前方一致: ソート済み配列の2分探索
//! - 後方一致 / 部分一致 / あいまい(レーベンシュタイン距離): 全走査
//!   (チャンネル単位で数千語程度を想定しているため十分)
//!
//! 永続化は RoomManager 側のデータファイル(rooms JSON)へ単語配列として書き出される。
//! WordStore 自体は `Serialize/Deserialize` に対応しており、
//! 単語リスト(ソート済み・重複なし)を透過的に入出力する。

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::lev::levenshtein;

/// 単語検索モード
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// 完全一致
    Exact,
    /// 前方一致 (双方向: 保存語がqueryの前方一致 / queryが保存語の前方一致)
    Prefix,
    /// 後方一致 (双方向)
    Suffix,
    /// 部分一致 (双方向)
    Substring,
    /// あいまい検索 (レーベンシュタイン距離 <= max_dist)
    Fuzzy,
}

/// あいまい検索の既定最大距離(v3互換: 文字数の1/4)
pub fn default_fuzzy_max_dist(query: &str) -> usize {
    query.chars().count() / 4
}

#[derive(Debug, Clone, Default)]
pub struct WordStore {
    /// ソート済み・重複なし・正規化済み(lowercase)の単語リスト
    words: Vec<String>,
    /// 完全一致判定用のセット
    set: HashSet<String>,
}

impl WordStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 単語を正規化する(trim + lowercase + 連続空白の圧縮)
    ///
    /// フレーズ(例: "donkey kong")を1つのエントリとして扱うため、
    /// 前後の空白を除去し、2連続以上の空白(タブ等含む)は1つの半角スペースに畳む。
    pub fn normalize(word: &str) -> String {
        word.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 正規化済みの単語をすべて返す(ソート済み)
    pub fn words(&self) -> &[String] {
        &self.words
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// 完全一致判定(入力は正規化する)
    pub fn contains(&self, word: &str) -> bool {
        self.set.contains(&Self::normalize(word))
    }

    /// 単語を追加する。すでに存在する場合は false を返す。
    pub fn add_word(&mut self, word: &str) -> bool {
        let normalized = Self::normalize(word);
        if normalized.is_empty() || !self.set.insert(normalized.clone()) {
            return false;
        }

        // ソート位置に挿入して順序を保つ
        let pos = self.words.binary_search(&normalized).unwrap_or_else(|p| p);
        self.words.insert(pos, normalized);
        true
    }

    /// 単語を削除する。実際に削除された場合 true(正規化した完全一致)
    pub fn remove_word(&mut self, word: &str) -> bool {
        let normalized = Self::normalize(word);
        if !self.set.remove(&normalized) {
            return false;
        }
        if let Ok(pos) = self.words.binary_search(&normalized) {
            self.words.remove(pos);
        }
        true
    }

    /// 複数単語を追加し、実際に追加された単語を返す
    pub fn add_words<I: IntoIterator>(&mut self, words: I) -> Vec<String>
    where
        I::Item: AsRef<str>,
    {
        words
            .into_iter()
            .filter(|w| self.add_word(w.as_ref()))
            .map(|w| Self::normalize(w.as_ref()))
            .collect()
    }

    /// 検索。結果はソート済みで重複なし。
    pub fn search(&self, query: &str, mode: SearchMode) -> Vec<String> {
        self.search_with_dist(query, mode, default_fuzzy_max_dist(query))
    }

    /// 最大距離を明示して検索(Fuzzy以外では max_dist は使用されない)
    pub fn search_with_dist(&self, query: &str, mode: SearchMode, max_dist: usize) -> Vec<String> {
        let query = Self::normalize(query);
        if query.is_empty() {
            return vec![];
        }

        let mut results = match mode {
            SearchMode::Exact => {
                if self.set.contains(&query) {
                    vec![query]
                } else {
                    vec![]
                }
            }
            SearchMode::Prefix => {
                // ソート済み配列から query 以降で始まる範囲を2分探索で取得
                let start = self.words.binary_search(&query).unwrap_or_else(|p| p);
                let end = self.words[start.min(self.words.len())..]
                    .partition_point(|w| w.starts_with(&query))
                    + start.min(self.words.len());

                let mut found: Vec<String> = self.words[start..end].to_vec();
                // 双方向: 保存語が query の前方一致ではなく、query が保存語の前方一致
                // (例: query="cats" で保存語 "cat" にも一致させる)
                found.extend(
                    self.words[..start]
                        .iter()
                        .filter(|w| query.starts_with(w.as_str()))
                        .cloned(),
                );
                found
            }
            SearchMode::Suffix => self
                .words
                .iter()
                .filter(|w| w.ends_with(&query) || query.ends_with(w.as_str()))
                .cloned()
                .collect(),
            SearchMode::Substring => self
                .words
                .iter()
                .filter(|w| w.contains(&query) || query.contains(w.as_str()))
                .cloned()
                .collect(),
            SearchMode::Fuzzy => self.search_fuzzy(&query, max_dist),
        };

        // 保険としてソート+重複排除
        results.sort();
        results.dedup();

        results
    }

    /// あいまい検索(レーベンシュタイン距離 <= max_dist)。
    /// 文字数差の事前枝刈り付き全走査。
    pub fn search_fuzzy(&self, query: &str, max_dist: usize) -> Vec<String> {
        let query = Self::normalize(query);
        if query.is_empty() {
            return vec![];
        }

        let query_len = query.chars().count();

        self.words
            .iter()
            // 文字数差が max_dist を超える語は絶対に距離が超えるため除外(枝刈り)
            .filter(|w| {
                let count = w.chars().count();
                count.abs_diff(query_len) <= max_dist
            })
            .filter(|w| levenshtein(&query, w) <= max_dist)
            .cloned()
            .collect()
    }
}

/// 保存形式: ソート済み単語配列の透過的な入出力
impl Serialize for WordStore {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.words.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WordStore {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<String>::deserialize(deserializer).map(|words| {
            let mut store = WordStore::new();
            for w in words {
                store.add_word(&w);
            }
            store
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{SearchMode, WordStore, default_fuzzy_max_dist};

    fn store() -> WordStore {
        let mut s = WordStore::new();
        s.add_words([
            "apple", "banana", "grape", "apply", "apples", "cater", "sate",
        ]);
        s
    }

    #[test]
    fn normalize_lowercases_and_trims() {
        let mut s = WordStore::new();
        assert!(s.add_word("  Apple "));
        // 正規化により重複確認・一致判定が動く
        assert!(s.contains("apple"));
        assert!(s.contains(" APPLE "));
        assert!(!s.add_word("apple"));
        assert_eq!(s.words(), &["apple".to_string()]);
    }

    #[test]
    fn normalize_collapses_whitespace() {
        // レスポンスの正規化: trim + lowercase + 連続空白圧縮
        assert_eq!(WordStore::normalize("  Donkey   kONG  "), "donkey kong");
        assert_eq!(WordStore::normalize("\tdonkey\tkong"), "donkey kong");
        // 完全一致判定も同一正規化で行われる
        let mut s = WordStore::new();
        s.add_word("donkey kong");
        assert!(s.contains("DONKEY  KONG"));
        assert!(!s.add_word("donkey   kong"));
        assert_eq!(s.words(), &["donkey kong".to_string()]);
    }

    #[test]
    fn add_word_dedupes_and_sorts() {
        let mut s = WordStore::new();
        s.add_word("grape");
        s.add_word("apple");
        s.add_word("apple");
        assert_eq!(s.words(), &["apple".to_string(), "grape".to_string()]);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn empty_word_rejected() {
        let mut s = WordStore::new();
        assert!(!s.add_word("   "));
        assert!(s.is_empty());
    }

    #[test]
    fn search_exact() {
        let s = store();
        assert_eq!(s.search("apple", SearchMode::Exact), vec!["apple"]);
        assert!(s.search("appl", SearchMode::Exact).is_empty());
    }

    #[test]
    fn search_prefix_both_directions() {
        let s = store();
        // query が保存語の前方: apple, apply, apples (ソート済み)
        let apple_group = s.search("app", SearchMode::Prefix);
        assert_eq!(apple_group, vec!["apple", "apples", "apply"]);
        // 保存語が query の前方: "apples" に対して "apple" も出る
        let apples_group = s.search("apples", SearchMode::Prefix);
        assert!(apples_group.contains(&"apple".to_string()));
        assert!(apples_group.contains(&"apples".to_string()));
    }

    #[test]
    fn search_suffix() {
        let s = store();
        // "ate" の後方一致は "sate" のみ("grape"は"ape"しか一致しない)
        let hits = s.search("ate", SearchMode::Suffix);
        assert_eq!(hits, vec!["sate"]);
        // query側の語尾にも一致する(双方向): query "jcater" は語尾が "cater" と一致
        let hits2 = s.search("jcater", SearchMode::Suffix);
        assert!(hits2.contains(&"cater".to_string()));
    }

    #[test]
    fn search_substring() {
        let s = store();
        let hits = s.search("anana", SearchMode::Substring);
        assert_eq!(hits, vec!["banana"]);
        // queryが Part する語にも一致する(双方向)
        let cat = s.search("caterp", SearchMode::Substring);
        assert!(cat.contains(&"cater".to_string()));
    }

    #[test]
    fn search_fuzzy() {
        let s = store();
        // "aple" のレーベンシュタイン距離1で "apple"
        let hits = s.search("aple", SearchMode::Fuzzy);
        assert!(hits.contains(&"apple".to_string()));
        // max_dist 0 なら距離0のみ
        let hits0 = s.search_with_dist("apple", SearchMode::Fuzzy, 0);
        assert_eq!(hits0, vec!["apple"]);
    }

    #[test]
    fn fuzzy_prunes_by_length_diff() {
        let mut s = WordStore::new();
        s.add_word("supercalifragilisticexpialidocious");
        // 文字数差が大きすぎるのでクエリ "x" では出ない
        assert!(s.search_fuzzy("x", 1).is_empty());
    }

    #[test]
    fn empty_query_returns_empty() {
        let s = store();
        assert!(s.search("", SearchMode::Fuzzy).is_empty());
        assert!(s.search("   ", SearchMode::Prefix).is_empty());
    }

    #[test]
    fn default_fuzzy_max_dist_is_quarter() {
        assert_eq!(default_fuzzy_max_dist("abcd"), 1);
        assert_eq!(default_fuzzy_max_dist("abcde"), 1);
        assert_eq!(default_fuzzy_max_dist("abcdefgh"), 2);
        assert_eq!(default_fuzzy_max_dist("ab"), 0);
    }

    #[test]
    fn serde_roundtrip() {
        let s = store();
        let json = serde_json::to_string(&s).unwrap();
        let restored: WordStore = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.words(), s.words());
    }

    #[test]
    fn serde_from_dupes_and_broken_case_renormalizes() {
        let json = r#"["Apple","apple","GRAPE"]"#;
        let s: WordStore = serde_json::from_str(json).unwrap();
        assert_eq!(s.words(), &["apple".to_string(), "grape".to_string()]);
    }
}
