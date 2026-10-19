//! v4 データモデルと永続化
//!
//! データファイル形式: バージョン付き単一JSON (`shiritori-data`)
//!
//! ```json
//! {
//!   "format": "shiritori-data",
//!   "version": 1,
//!   "saved_at": 1760000000,
//!   "rooms": {
//!     "123456789012345678": {
//!       "user_queue": [111, 222],
//!       "words": ["apple", "grape"],
//!       "vote": {
//!         "message_id": 999,
//!         "word": "apple",
//!         "user_id": 111,
//!         "good": [222],
//!         "bad": []
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! - 人間可読(JSON)で、バージョンが先頭に付くため将来の形式変更に耐える
//! - 未知フィールドは無視される(std::serdeデフォルト)、version不一致は警告ログのみで続行
//! - 保存は tmp ファイル → rename のアトミック書き込み
//! - 上書き前に既存ファイルを `.bak` として退避する

use std::{collections::HashMap, fs, path::Path, sync::Arc};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::words::WordStore;

/// フォーマット識別子
pub const FORMAT_ID: &str = "shiritori-data";
/// 現在のフォーマットバージョン
pub const FORMAT_VERSION: u32 = 1;

/// 進行中の投票データ
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoteData {
    /// 投票対象メッセージID
    pub message_id: u64,
    /// 回答単語
    pub word: String,
    /// 回答者
    pub user_id: u64,
    /// 賛成(👍)投票者
    pub good: Vec<u64>,
    /// 反対(👎)投票者
    pub bad: Vec<u64>,
}

/// ルーム1つ分の永続データ
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoomData {
    /// 表示名(主に サーバー名/チャンネル名)。なければ省略
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// ユーザーの回答順
    pub user_queue: Vec<u64>,
    /// 辞書(回答済み単語)
    pub words: WordStore,
    /// 進行中の投票(なければ省略)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vote: Option<VoteData>,
}

/// 保存ファイル全体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveData {
    pub format: String,
    pub version: u32,
    /// 保存時刻 (Unixエポック秒)
    pub saved_at: i64,
    pub rooms: HashMap<u64, RoomData>,
}

impl Default for SaveData {
    fn default() -> Self {
        Self::new()
    }
}

impl SaveData {
    pub fn new() -> Self {
        Self {
            format: FORMAT_ID.to_string(),
            version: FORMAT_VERSION,
            saved_at: Utc::now().timestamp(),
            rooms: HashMap::new(),
        }
    }

    /// JSON文字列を v4 形式として解析・検証する
    ///
    /// - format不一致はエラー
    /// - version差異は警告ログのみで続行(読み込み互換性を優先)
    pub fn decode(json: &str) -> Result<SaveData, LoadError> {
        let data: SaveData = serde_json::from_str(json)?;
        if data.format != FORMAT_ID {
            return Err(LoadError::FormatMismatch(data.format));
        }
        if data.version > FORMAT_VERSION {
            println!(
                "[警告] データファイルのバージョン({})が現在({})より新しいです。読み込んで続行します。",
                data.version, FORMAT_VERSION
            );
        } else if data.version < FORMAT_VERSION {
            println!(
                "[警告] データファイルのバージョン({})が現在({})より古いです。",
                data.version, FORMAT_VERSION
            );
        }
        Ok(data)
    }
}

/// ランタイムのルーム状態
#[derive(Debug, Clone, Default)]
pub struct RoomState {
    pub room_id: u64,
    /// 表示名(主に サーバー名/チャンネル名)
    pub name: Option<String>,
    pub user_queue: Vec<u64>,
    pub words: WordStore,
    pub vote: Option<VoteData>,
}

impl RoomState {
    pub fn new(room_id: u64) -> Self {
        Self {
            room_id,
            ..Default::default()
        }
    }

    /// 永続データへ変換
    fn to_data(&self) -> RoomData {
        RoomData {
            name: self.name.clone(),
            user_queue: self.user_queue.clone(),
            words: self.words.clone(),
            vote: self.vote.clone(),
        }
    }

    /// 永続データから復元
    fn from_data(room_id: u64, data: RoomData) -> Self {
        Self {
            room_id,
            name: data.name,
            user_queue: data.user_queue,
            words: data.words,
            vote: data.vote,
        }
    }
}

impl From<&RoomState> for RoomData {
    fn from(room: &RoomState) -> Self {
        room.to_data()
    }
}

/// 全ルーム管理 + 永続化
///
/// - 内部はルームID → `Arc<RwLock<RoomState>>` のマップ
/// - 保存は書き込みロックを取得した状態で完結する(v3の `try_read` 失敗問題の解消)
#[derive(Debug, Clone, Default)]
pub struct RoomManager {
    pub rooms: Arc<RwLock<HashMap<u64, Arc<RwLock<RoomState>>>>>,
}

impl RoomManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// ルームを取得、なければ新規作成して Arc<RwLock<RoomState>> を返す
    pub async fn get_or_new_room(&self, room_id: u64) -> Arc<RwLock<RoomState>> {
        // 1. 読み取りで存在チェック
        if let Some(room) = self.rooms.read().await.get(&room_id) {
            return room.clone();
        }

        // 2. 書き込みロックで新規作成(他タスクが作成済みの場合はそれを使う)
        let mut rooms = self.rooms.write().await;
        rooms
            .entry(room_id)
            .or_insert_with(|| Arc::new(RwLock::new(RoomState::new(room_id))))
            .clone()
    }

    /// ルームが登録済みかどうか
    pub async fn has_room(&self, room_id: u64) -> bool {
        self.rooms.read().await.contains_key(&room_id)
    }

    /// 登録済みルームID一覧
    pub async fn room_ids(&self) -> Vec<u64> {
        self.rooms.read().await.keys().copied().collect()
    }

    /// ルームの表示名を設定する(部屋が無ければ作成する)
    pub async fn set_room_name(&self, room_id: u64, name: String) {
        let room_arc = self.get_or_new_room(room_id).await;
        room_arc.write().await.name = Some(name);
    }

    /// ルームを削除する(辞書・キュー・投票ごと消える)。存在した場合は true
    pub async fn delete_room(&self, room_id: u64) -> bool {
        self.rooms.write().await.remove(&room_id).is_some()
    }

    /// 全ルームの (room_id, 表示名) 一覧。名前付きルームを表示名順、
    /// 未登録ルームは ID の文字列でその後に並べる
    pub async fn room_display(&self) -> Vec<(u64, String)> {
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
        enum Key {
            Named(String, u64),
            Unnamed(u64),
        }

        let rooms = self.rooms.read().await;
        let mut list = Vec::with_capacity(rooms.len());
        for (id, arc) in rooms.iter() {
            let name = arc.read().await.name.clone();
            match name {
                Some(name) => list.push((Key::Named(name.to_lowercase(), *id), (*id, name))),
                None => list.push((Key::Unnamed(*id), (*id, id.to_string()))),
            }
        }
        drop(rooms);
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list.into_iter().map(|(_, pair)| pair).collect()
    }

    /// 全ルームを与えられたSaveDataで置き換える(v4形式import用)
    pub async fn replace_with(&self, data: SaveData) {
        let rooms: HashMap<u64, Arc<RwLock<RoomState>>> = data
            .rooms
            .into_iter()
            .map(|(room_id, room_data)| {
                let room = RoomState::from_data(room_id, room_data);
                (room_id, Arc::new(RwLock::new(room)))
            })
            .collect();

        *self.rooms.write().await = rooms;
    }

    /// 現在の状態を SaveData にスナップショットする
    pub async fn snapshot(&self) -> SaveData {
        let mut data = SaveData::new();

        let rooms = self.rooms.read().await;
        for (room_id, room_arc) in rooms.iter() {
            let room = room_arc.read().await;
            data.rooms.insert(*room_id, room.to_data());
        }

        data
    }

    /// 起動時のサマリ(ルーム数・辞書語数)を1行で返す。load 後の状態確認用
    async fn summarize_loaded(&self) -> String {
        let room_count;
        let mut words = 0usize;
        {
            let rooms = self.rooms.read().await;
            room_count = rooms.len();
            for arc in rooms.values() {
                words += arc.read().await.words.len();
            }
        }
        format!("ルーム{room_count}件 / 辞書{words}語")
    }

    /// ファイルへ保存する(アトミック書き込み + 直前の状態を .bak に退避)
    ///
    /// スナップショットはすでに取得済みのものを渡す(ロック保持とIOの分離)。
    pub fn write_file(data: &SaveData, path: &str) -> std::io::Result<()> {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)?;
        }

        // 既存ファイルがあれば .bak に退避
        if Path::new(path).exists() {
            let bak = format!("{path}.bak");
            fs::rename(path, &bak)?;
        }

        // tmp へ書き出してからリネーム(中断しても不整合を起こさない)
        let tmp = format!("{path}.tmp");
        let json = serde_json::to_string_pretty(data)?;
        fs::write(&tmp, json.as_bytes())?;
        fs::rename(&tmp, path)?;

        Ok(())
    }

    /// スナップショットをファイルに保存する(同期IO。単一JSONのため高速)
    pub async fn save_file(&self, path: &str) -> std::io::Result<()> {
        let data = self.snapshot().await;
        Self::write_file(&data, path)
    }

    /// ファイルから読み込む。
    ///
    /// - 未知フィールドはserdeにより無視される(将来の互換性)
    /// - version が現在より新しい場合は警告を出して続行(後方互換優先)
    /// - data.json の解析に失敗した場合/ファイルが欠損している場合は
    ///   `data.json.bak` からの自動復帰を試みる(保存は rename による世代交代なので
    ///   .bak に常に直近の完全な状態が残る)
    /// - 原文が壊れていた場合は `data.json.corrupt` に退避してからエラーを返す
    /// - 成功時はルーム数/辞書語数のサマリを出力する
    pub async fn load_file(path: &str) -> Result<Self, LoadError> {
        let json = match fs::read_to_string(path) {
            Ok(json) => json,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Self::recover_from_bak(path, "データファイルが存在しません", None).await;
            }
            Err(e) => return Err(LoadError::Io(e)),
        };

        match Self::parse(&json) {
            Ok(data) => {
                let manager = Self::build(data);
                println!("[データ] {}", manager.summarize_loaded().await);
                Ok(manager)
            }
            Err(e) => {
                // 壊れた原文を退避してから .bak で復帰する
                let corrupt = format!("{path}.corrupt");
                let _ = fs::write(&corrupt, json.as_bytes());
                eprintln!("[警告] 読み込みに失敗({e})。原文を {corrupt} に退避しました。");
                Self::recover_from_bak(path, &format!("data.jsonの解析に失敗({e})"), Some(e)).await
            }
        }
    }

    /// .bak からの復帰。
    ///
    /// - bak から復帰できた場合は警告+サマリつきで返す
    /// - bak も存在しない場合: 本体も無ければ初回起動として Ok(空)、
    ///   本体の解析エラーがあればそのエラーを Err で返す(空状態での起動は呼び出し元の責務)
    /// - bak はあるが解析できない場合: 元エラー(または FormatMismatch)を返す
    async fn recover_from_bak(
        path: &str,
        reason: &str,
        original: Option<LoadError>,
    ) -> Result<Self, LoadError> {
        let bak = format!("{path}.bak");
        match fs::read_to_string(&bak) {
            Ok(json) => match Self::parse(&json) {
                Ok(data) => {
                    let manager = Self::build(data);
                    eprintln!(
                        "[警告] {reason} → {bak} から復旧しました。{}",
                        manager.summarize_loaded().await
                    );
                    Ok(manager)
                }
                Err(e) => {
                    eprintln!("[警告] {reason} → {bak} も解析に失敗({e})。空の状態で起動します。");
                    Err(original.unwrap_or(LoadError::FormatMismatch(bak)))
                }
            },
            Err(io) if io.kind() == std::io::ErrorKind::NotFound => match original {
                Some(e) => {
                    eprintln!(
                        "[警告] {reason} → {bak} も存在せず復帰できません。空の状態で起動します。"
                    );
                    Err(e)
                }
                None => Ok(Self::new()),
            },
            Err(e) => Err(LoadError::Io(e)),
        }
    }

    /// JSON文字列を解析する。format不一致はエラー、version差異は警告のみで続行
    fn parse(json: &str) -> Result<SaveData, LoadError> {
        SaveData::decode(json)
    }

    /// SaveDataからランタイムのマネージャーを構築する
    fn build(data: SaveData) -> Self {
        let rooms: HashMap<u64, Arc<RwLock<RoomState>>> = data
            .rooms
            .into_iter()
            .map(|(room_id, room_data)| {
                let room = RoomState::from_data(room_id, room_data);
                (room_id, Arc::new(RwLock::new(room)))
            })
            .collect();

        Self {
            rooms: Arc::new(RwLock::new(rooms)),
        }
    }
}

/// 読み込みエラー
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error("未知のデータフォーマット: {0}")]
    FormatMismatch(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vote_sample() -> VoteData {
        VoteData {
            message_id: 999,
            word: "apple".into(),
            user_id: 111,
            good: vec![222, 333],
            bad: vec![],
        }
    }

    fn room_sample() -> RoomData {
        let mut words = WordStore::new();
        words.add_words(["apple", "grape"]);
        RoomData {
            name: Some("Test Server/test-room".to_string()),
            user_queue: vec![111, 222, 333],
            words,
            vote: Some(vote_sample()),
        }
    }

    #[test]
    fn save_data_roundtrip() {
        let mut data = SaveData::new();
        data.rooms.insert(123456, room_sample());

        let json = serde_json::to_string(&data).unwrap();
        let restored: SaveData = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.format, "shiritori-data");
        assert_eq!(restored.version, 1);
        let room = restored.rooms.get(&123456).unwrap();
        assert_eq!(room.user_queue, vec![111, 222, 333]);
        assert!(room.words.contains("apple"));
        assert!(room.words.contains("grape"));
        assert_eq!(room.words.len(), 2);
        let vote = room.vote.as_ref().unwrap();
        assert_eq!(vote.message_id, 999);
        assert_eq!(vote.word, "apple");
        assert_eq!(vote.user_id, 111);
        assert_eq!(vote.good, vec![222, 333]);
        assert!(vote.bad.is_empty());
    }

    #[test]
    fn json_keys_are_readable_strings() {
        let mut data = SaveData::new();
        data.rooms.insert(123456, room_sample());
        let json = serde_json::to_string(&data).unwrap();
        // ルームIDは文字列キーとして出力される(人間可読)
        assert!(json.contains("\"123456\""));
        assert!(json.contains("\"format\":\"shiritori-data\""));
    }

    #[test]
    fn vote_option_is_skipped_when_none() {
        let mut data = SaveData::new();
        data.rooms.insert(
            123,
            RoomData {
                name: None,
                user_queue: vec![],
                words: WordStore::new(),
                vote: None,
            },
        );
        let json = serde_json::to_string(&data).unwrap();
        assert!(!json.contains("\"vote\""));
    }

    #[tokio::test]
    async fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("shiritori_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("data.json").to_str().unwrap().to_string();

        let manager = RoomManager::new();
        let room = manager.get_or_new_room(111).await;
        {
            let mut r = room.write().await;
            r.user_queue = vec![1, 2, 3];
            r.words.add_word("grape");
            r.vote = Some(vote_sample());
        }
        manager.save_file(&path).await.unwrap();

        // .tmp は残らない
        assert!(!Path::new(&format!("{path}.tmp")).exists());

        let loaded = RoomManager::load_file(&path).await.unwrap();
        let room2 = loaded.get_or_new_room(111).await;
        let r2 = room2.read().await;
        assert_eq!(r2.user_queue, vec![1, 2, 3]);
        assert!(r2.words.contains("grape"));
        assert_eq!(r2.vote.as_ref().unwrap().word, "apple");
    }

    #[tokio::test]
    async fn backup_is_created_on_save() {
        let dir = std::env::temp_dir().join(format!("shiritori_test_bak_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("data.json").to_str().unwrap().to_string();

        let manager = RoomManager::new();
        let room = manager.get_or_new_room(1).await;
        room.write().await.user_queue = vec![42];
        manager.save_file(&path).await.unwrap();

        // 2回目の保存で .bak が作られる(初回状態が退避される)
        let room = manager.get_or_new_room(2).await;
        room.write().await.user_queue = vec![43];
        manager.save_file(&path).await.unwrap();

        let bak: SaveData =
            serde_json::from_str(&std::fs::read_to_string(format!("{path}.bak")).unwrap()).unwrap();
        assert!(bak.rooms.contains_key(&1));
        assert!(!bak.rooms.contains_key(&2));

        let current: SaveData =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(current.rooms.contains_key(&2));
        assert!(current.rooms.contains_key(&1));
    }

    #[tokio::test]
    async fn load_missing_file_returns_empty() {
        let manager = RoomManager::load_file("/nonexistent/dir/data.json")
            .await
            .unwrap();
        assert!(manager.snapshot().await.rooms.is_empty());
    }

    #[tokio::test]
    async fn newer_version_warns_but_loads() {
        let dir = std::env::temp_dir().join(format!("shiritori_test_ver_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("newer.json");

        let json = r#"{
            "format": "shiritori-data",
            "version": 99,
            "saved_at": 0,
            "unknown_future_field": true,
            "rooms": {
                "555": { "user_queue": [1, 2], "words": ["apple"] }
            }
        }"#;
        std::fs::write(&path, json).unwrap();

        let manager = RoomManager::load_file(path.to_str().unwrap())
            .await
            .unwrap();
        let room = manager.get_or_new_room(555).await;
        let r = room.read().await;
        assert_eq!(r.user_queue, vec![1, 2]);
        assert!(r.words.contains("apple"));
        assert!(r.vote.is_none());
    }

    #[tokio::test]
    async fn wrong_format_is_rejected() {
        let json = r#"{ "format": "something-else", "version": 1, "saved_at": 0, "rooms": {} }"#;
        let dir = std::env::temp_dir().join(format!("shiritori_test_fmt_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("wrong.json");
        std::fs::write(&path, json).unwrap();

        let result = RoomManager::load_file(path.to_str().unwrap()).await;
        assert!(matches!(result, Err(LoadError::FormatMismatch(_))));
    }

    #[tokio::test]
    async fn room_state_data_conversion() {
        let mut state = RoomState::new(777);
        state.user_queue = vec![10, 20];
        state.words.add_word("cat");
        state.vote = Some(vote_sample());

        let data = RoomData::from(&state);
        let restored = RoomState::from_data(777, data);

        assert_eq!(restored.room_id, 777);
        assert_eq!(restored.user_queue, vec![10, 20]);
        assert!(restored.words.contains("cat"));
        assert_eq!(restored.vote.unwrap().message_id, 999);
    }

    #[tokio::test]
    async fn set_room_name_and_display_order_fallback() {
        let manager = RoomManager::new();
        manager.set_room_name(2000, "Beta/gamma".to_string()).await;
        manager.set_room_name(1000, "Alpha/beta".to_string()).await;
        manager.get_or_new_room(3000).await; // 名前なしルーム

        let list = manager.room_display().await;
        assert_eq!(
            list,
            vec![
                (1000, "Alpha/beta".to_string()),
                (2000, "Beta/gamma".to_string()),
                (3000, "3000".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn room_name_survives_save_load() {
        let manager = RoomManager::new();
        manager
            .set_room_name(42, "MyServer/general".to_string())
            .await;
        let path = "/tmp/shiritori_test_data_name/data.json";
        manager.save_file(path).await.unwrap();

        let loaded = RoomManager::load_file(path).await.unwrap();
        assert_eq!(
            loaded.room_display().await,
            vec![(42, "MyServer/general".to_string())]
        );
    }

    #[test]
    fn room_data_without_name_loads_backcompat() {
        // v4 旧保存データ(名前フィールドなし)はそのまま読める
        let json = r#"{"user_queue":[],"words":[]}"#;
        let data: RoomData = serde_json::from_str(json).unwrap();
        assert!(data.name.is_none());
    }
    // ===== 起動時の自動復帰(.bak)と破損退避(.corrupt) =====

    /// 直近2世代がそろったテストデータを作る(seed語を1世代目のみに含める)
    async fn seed_two_generations(dir: &str) {
        use std::fs;
        fs::create_dir_all(dir).unwrap();
        let path = format!("{dir}/data.json");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(format!("{path}.bak"));

        // 1世代目: banana を含む状態で保存→.bakに退避される
        let m1 = RoomManager::new();
        let room = m1.get_or_new_room(100).await;
        room.write().await.words.add_word("banana");
        drop(room);
        m1.save_file(&path).await.unwrap();

        // 2世代目: apple を含む状態で保存(1世代目は.bakへ)
        let m2 = RoomManager::new();
        let room = m2.get_or_new_room(100).await;
        room.write().await.words.add_word("apple");
        drop(room);
        m2.save_file(&path).await.unwrap();
    }

    #[tokio::test]
    async fn corrupt_data_file_recovers_from_bak() {
        use std::fs;

        let dir = "/tmp/shiritori_data_recover";
        seed_two_generations(dir).await;

        let path = format!("{dir}/data.json");
        // 本体を壊す
        fs::write(&path, br#"not a json{{{"#).unwrap();

        let loaded = RoomManager::load_file(&path).await.unwrap();
        let room = loaded.get_or_new_room(100).await;
        assert!(
            room.read().await.words.contains("banana"),
            ".bak(1世代前)から復旧できている"
        );
        // 原文が.corruptに退避されている
        assert!(fs::metadata(format!("{path}.corrupt")).is_ok());

        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn wrong_format_data_file_recovers_from_bak() {
        use std::fs;

        let dir = "/tmp/shiritori_data_recover_fmt";
        seed_two_generations(dir).await;

        let path = format!("{dir}/data.json");
        // format が違うが JSON としては正当なデータ
        fs::write(
            &path,
            br#"{"format":"other-app","version":1,"saved_at":1,"rooms":{}}"#,
        )
        .unwrap();

        let loaded = RoomManager::load_file(&path).await.unwrap();
        let room = loaded.get_or_new_room(100).await;
        assert!(
            room.read().await.words.contains("banana"),
            ".bakから復旧できている"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn missing_data_file_recovers_from_bak() {
        use std::fs;

        let dir = "/tmp/shiritori_data_recover_missing";
        seed_two_generations(dir).await;

        let path = format!("{dir}/data.json");
        // 世代交代中のkill相当: 本体だけ消える
        fs::remove_file(&path).unwrap();

        let loaded = RoomManager::load_file(&path).await.unwrap();
        let room = loaded.get_or_new_room(100).await;
        assert!(
            room.read().await.words.contains("banana"),
            "本体欠損時も.bakから復旧できている"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn no_files_at_all_starts_empty() {
        use std::fs;

        let dir = "/tmp/shiritori_data_fresh";
        fs::create_dir_all(dir).unwrap();
        let path = format!("{dir}/data.json");

        let loaded = RoomManager::load_file(&path).await.unwrap();
        assert!(loaded.snapshot().await.rooms.is_empty());

        let _ = fs::remove_dir_all(dir);
    }
}
