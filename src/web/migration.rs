//! v3 旧形式データの読み込みと移行
//!
//! 旧形式:
//! - `rooms.json`: `Vec<RoomState>` (serde JSON、VoteStateにTryMessageBuilderを含む)
//! - `word_{room_id}.idx`: bincode 2.0 でエンコードされた `FuzzyIndex` (BK木)
//!
//! 移行方針(マージ型・保守的):
//! - 既存のv4データ(キュー・単語・投票)を壊さない
//!   - user_queue: 既存が空で旧データがあれば上書き設定
//!   - words: 既存辞書に追加マージ(過剰写像なし)
//!   - vote: 既存が無いかつ旧投票が進行中なら復元(メッセージはDiscord上に残存)

use std::collections::HashSet;

use bincode::{Decode, Encode};
use serde::Deserialize;
use thiserror::Error;

use crate::data::{RoomManager, RoomState, VoteData};

/// v3ルーム状態(rooms.json の1要素)
#[derive(Debug, Deserialize)]
pub struct OldRoomState {
    pub room_id: u64,
    #[serde(default)]
    pub user_queue: Option<Vec<u64>>,
    #[serde(default)]
    pub vote_state: Option<OldVoteState>,
}

/// v3投票状態
#[derive(Debug, Deserialize)]
pub struct OldVoteState {
    #[serde(default)]
    pub target_user: Option<u64>,
    #[serde(default)]
    pub target_word: Option<String>,
    #[serde(default)]
    pub vote_message: Option<u64>,
    #[serde(default)]
    pub good_users: Option<HashSet<u64>>,
    #[serde(default)]
    pub bad_users: Option<HashSet<u64>>,
}

impl OldVoteState {
    /// v4形式のVoteDataに変換(必要項目が揃っている場合のみ)
    pub fn to_vote_data(&self) -> Option<VoteData> {
        Some(VoteData {
            message_id: self.vote_message?,
            word: self.target_word.as_ref()?.to_lowercase(),
            user_id: self.target_user?,
            good: self
                .good_users
                .as_ref()
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default(),
            bad: self
                .bad_users
                .as_ref()
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default(),
        })
    }
}

/// v3のBK木ノード(デコード用)
#[derive(Encode, Decode)]
pub struct OldNode {
    word: String,
    children: std::collections::HashMap<u32, OldNode>,
}

/// v3のFuzzyIndex(デコード用)
#[derive(Encode, Decode)]
pub struct OldFuzzyIndex {
    #[allow(dead_code)]
    root: Option<OldNode>,
    words_set: HashSet<String>,
}

/// 移行結果の要約
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportSummary {
    /// 新規登録されたルーム数
    pub rooms_created: usize,
    /// 既存ルームに追加適用されたルーム数
    pub rooms_updated: usize,
    /// 追加された単語数
    pub words_added: usize,
    /// 復元された進行中投票数
    pub votes_restored: usize,
}

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("旧rooms.jsonのJSON解析に失敗: {0}")]
    Json(#[from] serde_json::Error),

    #[error("旧単語ファイル(.idx)のbincodeデコードに失敗: {0}")]
    Bincode(#[from] bincode::error::DecodeError),

    #[error("IOエラー: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

/// 旧wordファイル名(word_{id}.idx)からroom_idを取り出す
pub fn parse_v3_idx_filename(name: &str) -> Option<u64> {
    let stem = name.strip_prefix("word_")?.strip_suffix(".idx")?;
    stem.parse().ok()
}

/// 旧idxバイト列のbincodeデコード(FuzzyIndexのwords_setを取得)
pub fn decode_v3_words(bytes: &[u8]) -> Result<Vec<String>, MigrationError> {
    let (index, _len): (OldFuzzyIndex, usize) =
        bincode::decode_from_slice(bytes, bincode::config::standard())?;
    Ok(index.words_set.into_iter().collect())
}

/// v3と同じbincode形式のidxデータを生成する(テスト・移行検証用)
#[doc(hidden)]
pub fn encode_v3_index_for_tests(words: &[&str]) -> Vec<u8> {
    let set: HashSet<String> = words.iter().map(|w| (*w).to_string()).collect();
    let index = OldFuzzyIndex {
        root: None,
        words_set: set,
    };
    let config = bincode::config::standard().with_little_endian();
    bincode::encode_to_vec(&index, config).unwrap()
}

/// 旧rooms.json の解析
pub fn decode_v3_rooms(json: &str) -> Result<Vec<OldRoomState>, MigrationError> {
    Ok(serde_json::from_str(json)?)
}

/// v3データを現在のRoomManagerへマージインポートする
///
/// - rooms_json: 旧rooms.jsonの文字列(None可)
/// - idx_files: (room_id, 単語リスト) のマージ対象(旧idxからdecode済み)
pub async fn import_v3(
    manager: &RoomManager,
    rooms_json: Option<&str>,
    word_lists: &[(u64, Vec<String>)],
) -> Result<ImportSummary, MigrationError> {
    let mut summary = ImportSummary::default();

    let old_rooms: Vec<OldRoomState> = match rooms_json {
        Some(json) => decode_v3_rooms(json)?,
        None => vec![],
    };

    for old in old_rooms {
        let is_new = !manager.has_room(old.room_id).await;
        let room_arc = manager.get_or_new_room(old.room_id).await;
        let mut room = room_arc.write().await;

        // キュー: 既存が空なら旧キューを設定
        if room.user_queue.is_empty()
            && let Some(queue) = &old.user_queue
        {
            if queue.is_empty() {
                room.user_queue.clear();
            } else {
                room.user_queue = queue.clone();
            }
        }

        // 投票: 既存が無い場合のみ復元
        if room.vote.is_none()
            && let Some(vote_state) = &old.vote_state
            && let Some(vote) = vote_state.to_vote_data()
        {
            room.vote = Some(vote);
            summary.votes_restored += 1;
        }

        if is_new {
            summary.rooms_created += 1;
        } else {
            summary.rooms_updated += 1;
        }
    }

    // 単語のマージ
    for (room_id, words) in word_lists {
        if words.is_empty() {
            continue;
        }
        let room_arc = manager.get_or_new_room(*room_id).await;
        let mut room: tokio::sync::RwLockWriteGuard<'_, RoomState> = room_arc.write().await;
        let added = room.words.add_words(words.iter().map(|w| w.as_str()));
        summary.words_added += added.len();
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::RoomData;

    /// 共有ヘルパ(migration::encode_v3_index_for_tests)を利用
    fn encode_v3_index(words: Vec<&str>) -> Vec<u8> {
        encode_v3_index_for_tests(&words)
    }

    #[test]
    fn decode_v3_words_from_bincode() {
        let bytes = encode_v3_index(vec!["apple", "grape", "apple"]);
        let words = decode_v3_words(&bytes).unwrap();
        assert_eq!(words.len(), 2);
        assert!(words.contains(&"apple".to_string()));
        assert!(words.contains(&"grape".to_string()));
    }

    #[test]
    fn decode_v3_words_garbage_input_fails() {
        assert!(decode_v3_words(&[0x00, 0x01, 0x02, 0x03]).is_err());
    }

    #[test]
    fn parse_filename_extracts_room_id() {
        assert_eq!(parse_v3_idx_filename("word_12345.idx"), Some(12345));
        assert_eq!(parse_v3_idx_filename("rooms.json"), None);
        assert_eq!(parse_v3_idx_filename("word_xx.idx"), None);
    }

    #[test]
    fn old_vote_state_conversion() {
        let mut good = HashSet::new();
        good.insert(111);
        let bad = HashSet::from([222, 333]);
        let old = OldVoteState {
            target_user: Some(11),
            target_word: Some("Apple".to_string()),
            vote_message: Some(999),
            good_users: Some(good),
            bad_users: Some(bad),
        };
        let vote = old.to_vote_data().unwrap();
        assert_eq!(vote.message_id, 999);
        assert_eq!(vote.word, "apple"); // lowercase化
        assert_eq!(vote.user_id, 11);
        assert_eq!(vote.good, vec![111]);
        assert_eq!(vote.bad.len(), 2);

        // 必要項目欠落 → None
        let incomplete = OldVoteState {
            target_user: None,
            target_word: Some("apple".to_string()),
            vote_message: Some(999),
            good_users: None,
            bad_users: None,
        };
        assert!(incomplete.to_vote_data().is_none());
    }

    #[test]
    fn decode_v3_rooms_sample_json() {
        // v3のrooms.json相当(方向・vote_state は省略可能)
        let json = r#"[
            {
                "room_id": 123456,
                "user_queue": [1, 2, 3],
                "vote_state": {
                    "target_user": 1,
                    "target_word": "apple",
                    "vote_message": 888,
                    "good_users": [2],
                    "bad_users": [],
                    "message_builder": {
                        "user": 1,
                        "word": "apple",
                        "mean": null,
                        "all_users": [2, 3],
                        "vote_good": [],
                        "vote_bad": [],
                        "like_words": null
                    }
                }
            }
        ]"#;

        let rooms = decode_v3_rooms(json).unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room_id, 123456);
        assert_eq!(rooms[0].user_queue.as_ref().unwrap().len(), 3);
        let vote = rooms[0]
            .vote_state
            .as_ref()
            .unwrap()
            .to_vote_data()
            .unwrap();
        assert_eq!(vote.word, "apple");
        assert_eq!(vote.good, vec![2]); // message_builder は無視
    }

    #[tokio::test]
    async fn import_merges_into_existing_rooms() {
        let manager = RoomManager::new();
        // 既存v4ルーム(既に単語・キューあり)
        {
            let room_arc = manager.get_or_new_room(1000).await;
            let mut room = room_arc.write().await;
            room.user_queue = vec![5, 6];
            room.words.add_word("banana");
        }

        let rooms_json = r#"[
            { "room_id": 1000, "user_queue": [77, 88], "vote_state": {
                "target_user": 77, "target_word": "apple", "vote_message": 42,
                "good_users": [], "bad_users": []
            }},
            { "room_id": 2000, "user_queue": [9] }
        ]"#;

        let summary = import_v3(
            &manager,
            Some(rooms_json),
            &[
                (
                    1000,
                    vec!["apple".to_string(), "Apple".to_string(), "new1".to_string()],
                ),
                (2000, vec!["grape".to_string()]),
            ],
        )
        .await
        .unwrap();

        // 既存ルームのデータは保護される
        let room1 = manager.get_or_new_room(1000).await;
        let r1 = room1.read().await;
        assert_eq!(r1.user_queue, vec![5, 6]); // 空でないため旧キューは適用されない
        assert!(r1.words.contains("banana")); // 既存維持
        assert!(r1.words.contains("apple")); // 追加マージ
        assert!(r1.words.contains("new1"));
        assert_eq!(r1.words.len(), 3);
        let vote = r1.vote.as_ref().unwrap();
        assert_eq!(vote.message_id, 42);
        assert_eq!(vote.user_id, 77);
        drop(r1);

        let room2 = manager.get_or_new_room(2000).await;
        let r2 = room2.read().await;
        assert_eq!(r2.user_queue, vec![9]); // 空のため旧キュー適用
        assert!(r2.words.contains("grape"));
        drop(r2);

        assert_eq!(summary.rooms_created, 1);
        assert_eq!(summary.rooms_updated, 1);
        assert_eq!(summary.words_added, 3); // apple, new1, grape(AAppleは重複)
        assert_eq!(summary.votes_restored, 1);
    }

    /// RoomData 直接参照の確認(serializeが破損しないことの健全性チェック)
    #[test]
    fn room_data_serializable() {
        let mut words = crate::words::WordStore::new();
        words.add_word("cat");
        let data = RoomData {
            name: None,
            user_queue: vec![1],
            words,
            vote: None,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("cat"));
    }
}
