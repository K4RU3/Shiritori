//! Bot本体: 状態とDiscord抽象化
//!
//! - `Bot`: ルーム状態管理(`RoomManager`)と意味プロバイダを持つボット本体
//! - `Messenger`: Discord操作の抽象化(送信/編集/リアクション削除/ephemeral返信)
//!
//! エンジン([`crate::bot::engine`])はこの抽象のみに依存するため、
//! Discordなしで単体テストが可能。

pub mod engine;
pub mod handler;
pub mod message;

use std::{error::Error, fmt, sync::Arc};

use serenity::async_trait;

use crate::{data::RoomManager, mean::MeaningProvider};

/// Bot本体
#[derive(Clone)]
pub struct Bot {
    /// ルーム状態管理
    pub manager: RoomManager,
    /// 意味プロバイダ(差し替え可能)
    pub meaning: Arc<dyn MeaningProvider + Send + Sync>,
    /// Web辞書の公開URL(設定時のみ)
    pub web_url: Option<String>,
}

impl Bot {
    pub fn new(
        manager: RoomManager,
        meaning: Arc<dyn MeaningProvider + Send + Sync>,
        web_url: Option<String>,
    ) -> Self {
        Self {
            manager,
            meaning,
            web_url,
        }
    }
}

/// メッセンジャーエラー(Box化して含める)
#[derive(Debug)]
pub struct MessengerError(Box<dyn Error + Send + Sync>);

impl MessengerError {
    pub fn new(err: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self(err.into())
    }
}

impl fmt::Display for MessengerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for MessengerError {}

pub type MessengerResult<T> = Result<T, MessengerError>;

/// Discord操作の抽象化
///
/// 全メソッドはパニックせず Result を返す。ネットワーク失敗時でも
/// ボット本体の状態は破壊されない。
#[async_trait]
pub trait Messenger: Send + Sync {
    /// チャンネルに投稿し、メッセージIDを返す
    async fn send(&self, channel_id: u64, content: String) -> MessengerResult<u64>;

    /// 既存メッセージを編集する
    async fn edit(&self, channel_id: u64, message_id: u64, content: String) -> MessengerResult<()>;

    /// 特定ユーザーのリアクションを削除する
    async fn delete_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        user_id: u64,
        emoji: &str,
    ) -> MessengerResult<()>;

    /// 一時的(ephemeral)な返信(interaction応答など)
    async fn reply_ephemeral(&self, content: String) -> MessengerResult<()>;
}

/// 共有 メッセンジャー型
pub type SharedMessenger = Arc<dyn Messenger + Send + Sync>;
