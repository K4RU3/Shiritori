use serde::{Serialize, Deserialize};
use serenity::futures::future::join_all;
use thiserror::Error;
use std::{collections::{HashMap, HashSet}, path::PathBuf, sync::Arc, vec};
use tokio::{self, sync::RwLock};

use crate::{arc_rwlock, fuzzy_index::IndexError, message::TryMessageBuilder, SharedFuzzyIndex};

/// 投票状態
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VoteState {
    pub target_user: Option<u64>,       // 被投票ユーザー
    pub target_word: Option<String>,    // 対象単語
    pub vote_message: Option<u64>,      // 投票メッセージid
    pub good_users: HashSet<u64>,       // Good投票ユーザー
    pub bad_users: HashSet<u64>,        // Bad投票ユーザー

    #[serde(skip)]
    pub message_builder: Arc<RwLock<TryMessageBuilder>>,
}

/// ルームの状態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomState {
    pub room_id: u64,                   // discordルームID
    pub user_queue: Vec<u64>,           // ユーザー順序
    pub vote_state: VoteState,          // 投票状態

    #[serde(skip)]
    pub index: Option<SharedFuzzyIndex>,
}

#[derive(Debug, Error)]
pub enum SaveError {
    #[error("Index is not loaded")]
    NotLoaded,

    #[error(transparent)]
    Index(#[from] IndexError),
}

impl RoomState {
    pub fn new(room_id: u64) -> Self {
        Self {
            room_id,
            user_queue: vec![],
            vote_state: VoteState::default(),
            index: None
        }
    }

    pub async fn load_words(&mut self, root_path: &str) {
        let room_id = self.room_id;

        let root_path = root_path.to_string(); // 所有を持たせるためにコピー

        let index = tokio::task::spawn_blocking(move || {
            let path = PathBuf::from(format!("{}word_{}.idx", root_path, room_id));

            match SharedFuzzyIndex::load(path) {
                Ok(fuzzy_index) => fuzzy_index,
                Err(e) => {
                    println!("not found words file (id: {})\ncreate a new words file\n{}", room_id, e);
                    SharedFuzzyIndex::new()
                }
            }
        })
        .await
        .expect("Word loading task panicked");

        self.index = Some(index);
    }

    pub async fn save_words(&mut self, root_path: &str) -> Result<(), SaveError> {
        if self.index.is_none() {
            return Err(SaveError::NotLoaded);
        }

        let root_path = root_path.to_string();

        let path = PathBuf::from(format!("{}word_{}.idx", root_path, self.room_id));

        let index = self.index.as_ref().unwrap();

        index.save(path).await?;
        Ok(())
    }
}

/// 全ルーム管理
#[derive(Debug, Clone, Default)]
pub struct RoomManager {
    pub rooms: Arc<RwLock<HashMap<u64, Arc<RwLock<RoomState>>>>>,
}

impl RoomManager {
    pub fn new() -> Self {
        Self { rooms: arc_rwlock!(HashMap::new()) }
    }

    pub async fn create_room(&mut self, room_id: u64) {
        let mut rooms_lock = self.rooms.write().await;
        rooms_lock.entry(room_id).or_insert(arc_rwlock!(RoomState::new(room_id)));
    }

    pub async fn get_room(&self, room_id: u64) -> Option<RoomState> {
        let rooms_lock = self.rooms.read().await;
        if let Some(room_ref) = rooms_lock.get(&room_id) {
            let room_lock = room_ref.read().await;
            return Some(room_lock.clone());
        }

        return None;
    }

    pub async fn get_room_mut(&self, room_id: u64) -> Option<Arc<RwLock<RoomState>>> {
        let rooms_lock = self.rooms.write().await;
        rooms_lock.get(&room_id).cloned()
    }

    pub async fn has_room(&self, room_id: u64) -> bool {
        let rooms_lock = self.rooms.read().await;
        rooms_lock.contains_key(&room_id)
    }

    pub async fn save_and_unload_all_room(&self, path: &str) {
        // rooms をロックして HashMap の値を一旦取り出す
        let rooms_to_save: Vec<Arc<RwLock<RoomState>>> = {
        let mut rooms = self.rooms.write().await;
            rooms.drain().map(|(_, room)| room).collect()
        };

        // すべての保存処理を非同期で走らせる
        let tasks = rooms_to_save.into_iter().map(|room_arc| async move {
            let mut room = room_arc.write().await;
            if let Err(e) = room.save_words(path).await {
                eprintln!("Failed to save room {}: {:?}", room.room_id, e);
            }
        });

        // join_all で並列実行
        join_all(tasks).await;
    }

    pub async fn save_all_words(&self, path: &str) {
        let rooms = self.rooms.read().await;

        // すべての RoomState への Arc を収集
        let futures: Vec<_> = rooms
            .values()
            .map(|room_arc| {
                let room = room_arc.clone();
                async move {
                    let mut room = room.write().await;
                    let _ = room.save_words(path).await;
                }
            })
            .collect();

        // すべての非同期タスクを同時実行
        join_all(futures).await;
    }

    pub async fn to_json(&self) -> String {
        let rooms_lock = self.rooms.read().await;

        // 各 room_arc を clone して Vec<RoomState> に集める
        let mut result = Vec::new();
        for room_arc in rooms_lock.values() {
            let room = room_arc.read().await;
            result.push(room.clone()); // RoomState: Clone が必要
        }

        serde_json::to_string(&result).unwrap()
    }

    pub async fn from_json(json: &str) -> Self {
        let map: HashMap<u64, RoomState> = serde_json::from_str(json).unwrap();

        let wrapped: HashMap<u64, Arc<RwLock<RoomState>>> = map
            .into_iter()
            .map(|(id, room)| (id, Arc::new(RwLock::new(room))))
            .collect();

        Self {
            rooms: Arc::new(RwLock::new(wrapped)),
        }
    }

    pub async fn save_to_file(&self, path: &str) -> tokio::io::Result<()> {
        let json_str = self.to_json();          // String を作成
        tokio::fs::write(path, json_str.await.as_bytes()).await
    }

    pub async fn load_or_new(path: &str) -> Self {
        match tokio::fs::read_to_string(path).await {
            Ok(json) => RoomManager::from_json(&json).await,
            Err(_) => RoomManager::new(),
        }
    }

    pub async fn save_all(&mut self, rooms_path: &str, words_path: &str) {
        let _ = self.save_to_file(rooms_path).await;
        self.save_all_words(words_path).await;
    }
}
