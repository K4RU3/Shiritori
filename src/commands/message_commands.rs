use std::sync::Arc;

use regex::Regex;
use serenity::all::{Context, GuildId, Message};
use tokio::sync::RwLock;

use crate::{actions::{add_queue, add_words, find_words, set_queue, skip, try_word, BotContext, SearchOptions}, message::generate_register_message, room::RoomManager, MatchMode};

use crate::commands::*;

lazy_static::lazy_static! {
    static ref REGEX_TRY: Regex = Regex::new(r"^[a-zA-Z\-\s]+$").unwrap();
    static ref REGEX_MENTION: Regex = Regex::new(r"<@!?(\d+)>").unwrap(); // <@123> または <@!123>
}

/// メンション文字列を Vec<u64> に変換
fn parse_mentions(msg: &Message) -> Vec<u64> {
    REGEX_MENTION
        .captures_iter(&msg.content)
        .filter_map(|caps| caps.get(1))
        .filter_map(|m| m.as_str().parse::<u64>().ok())
        .collect()
}

pub async fn handle(ctx: Context, msg: Message, bot_ctx: BotContext, manager: Arc<RwLock<RoomManager>>) {
    // ボットにメンション → そのチャンネルを対象ルームとして作成
    if msg.mentions_me(&ctx).await.unwrap_or(false) {
        let room_id = msg.channel_id.get();
        let mut is_resistered = false;
        let mut manager = manager.write().await; // 書き込みロック
        if manager.has_room(room_id).await {
            is_resistered = true;
        } else {
            manager.create_room(room_id).await;
        }

        // コマンドを登録
        if let Some(guild_id) = msg.guild_id {
            register_commands(&ctx, guild_id).await;
        }

        let message = generate_register_message(is_resistered);
        (bot_ctx.send)(message).await;
        return;
    }

    // コマンドは追加されたチャンネルのみで有効
    {
        let manager_lock = manager.read().await;
        if !manager_lock.has_room(bot_ctx.room_id).await {
            return;
        }
    }

    // try_word の正規表現マッチ
    if REGEX_TRY.is_match(&msg.content) {
        let word = msg.content.to_lowercase();
        let manager_read = manager.read().await; // 読み取りロック
        try_word(&bot_ctx, &manager_read, msg.author.id.get(), &word, msg.id.get()).await;
        return;
    }

    // find_words コマンド（先頭 ?）
    if msg.content.starts_with("?") {
        let query = msg.content[1..].trim();
        let manager_read = manager.read().await;
        let options = SearchOptions {
            fuzzy_distance: Some(query.len() / 4),
            match_mode: Some(MatchMode::Substring),
        };
        find_words(&bot_ctx, &manager_read, query, options, true).await;
        return;
    }

    // queue 設定系コマンド
    if msg.content.starts_with("!queue") {
        let mentions = parse_mentions(&msg);
        let mentions_u64: Vec<u64> = mentions.iter().map(|id| *id).collect();
        let manager_read = manager.read().await;
        set_queue(&bot_ctx, &manager_read, mentions_u64).await;
        return;
    }

    // queue 追加コマンド
    if msg.content.starts_with("!addqueue") {
        let mentions = parse_mentions(&msg);
        let manager_read = manager.read().await;
        for user_id in mentions {
            add_queue(&bot_ctx, &manager_read, user_id).await;
        }
        return;
    }

    // words 追加コマンド
    if msg.content.starts_with("!addwords") {
        // 1. "!addwords" を削除して前後の空白をトリム
        let trimmed = msg.content.trim_start_matches("!addwords").trim();

        // 2. 正規表現でチェック
        let re = Regex::new(r"^[a-zA-Z\s,]+$").unwrap();
        if !re.is_match(trimmed) {
            (bot_ctx.send)("入力入力形式が正しくありません。^!addwords [a-zA-Z\\s,]+$".to_string()).await;
            return;
        }

        // 3. カンマで分割、trim + lowercase
        let words: Vec<String> = trimmed
            .split(',')
            .map(|w| w.trim().to_lowercase())
            .filter(|w| !w.is_empty()) // 空文字列を除外
            .collect();

        // 4. 単語追加
        let mut manager_write = manager.write().await;
        add_words(&bot_ctx, &mut *manager_write, &words).await;
        return;
    }

    // skip コマンド
    if msg.content.starts_with("!skip") {
        let parts: Vec<&str> = msg.content.split_whitespace().collect();
        let mut len = 1;
        
        if parts.len() > 1 {
            len = parts[1].parse().unwrap_or(1);
        }

        let manager_read = manager.read().await; // 読み取りロック

        skip(&bot_ctx, &manager_read, len).await;
    }
}

// 特定のギルドにコマンドを追加
pub async fn register_commands(ctx: &Context, guild_id: GuildId) {
    let _ = guild_id.set_commands(&ctx.http, vec![
        vote::command(),
        find::command(),
        skip::command(),
    ]).await;
}