//! Serenity イベントハンドラとDiscord側の実装
//!
//! - [`DiscordMessenger`]: [`Messenger`] のDiscord実装
//! - [`Handler`]: message / reaction / interaction イベントをエンジンへ配線する
//!
//! 未登録チャンネルでは部屋を作らず、すべての操作を無視する。

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use lazy_static::lazy_static;
use regex::Regex;
use serenity::{
    all::{
        ChannelId, CommandInteraction, CreateCommand, CreateCommandOption,
        CreateInteractionResponse, CreateInteractionResponseFollowup,
        CreateInteractionResponseMessage, EditMessage, GuildId, Interaction, Message, MessageId,
        Reaction, ReactionType, Ready, UserId,
    },
    async_trait,
    prelude::*,
};

use crate::{
    bot::{Bot, Messenger, MessengerError, MessengerResult, SharedMessenger, engine, message as m},
    words::SearchMode,
};

lazy_static! {
    /// 英単語1語(単語を構成するのは英字とハイフンのみ)
    static ref REGEX_WORD: Regex = Regex::new(r"^[a-zA-Z][a-zA-Z\-]*$").unwrap();
    /// メンション表記
    static ref REGEX_MENTION: Regex = Regex::new(r"<@!?(\d+)>").unwrap();
}

/// Discordのメッセージ上限(2000字)に対する安全マージン
const MAX_TEXT_LEN: usize = 1990;

/// Discord用のMessenger実装
pub struct DiscordMessenger {
    ctx: Context,
    channel_id: ChannelId,
    interaction: Option<CommandInteraction>,
}

impl DiscordMessenger {
    /// チャンネル操作用(通常メッセージ)
    pub fn channel(ctx: &Context, channel_id: u64) -> SharedMessenger {
        Arc::new(Self {
            ctx: ctx.clone(),
            channel_id: ChannelId::new(channel_id),
            interaction: None,
        })
    }

    /// slashコマンド応答用(ephemeral返信が可能)
    pub fn from_interaction(ctx: &Context, interaction: &CommandInteraction) -> SharedMessenger {
        Arc::new(Self {
            ctx: ctx.clone(),
            channel_id: interaction.channel_id,
            interaction: Some(interaction.clone()),
        })
    }
}

/// 2000字上限に合わせて切り詰める
fn truncate_for_discord(text: String) -> String {
    if text.chars().count() <= MAX_TEXT_LEN {
        return text;
    }
    let mut cut: String = text.chars().take(MAX_TEXT_LEN).collect();
    cut.push_str("\n...(以降省略)");
    cut
}

#[async_trait]
impl Messenger for DiscordMessenger {
    async fn send(&self, _room_id: u64, content: String) -> MessengerResult<u64> {
        let sent = self
            .channel_id
            .say(&self.ctx.http, truncate_for_discord(content))
            .await
            .map_err(MessengerError::new)?;
        Ok(sent.id.get())
    }

    async fn edit(&self, _room_id: u64, message_id: u64, content: String) -> MessengerResult<()> {
        self.channel_id
            .edit_message(
                &self.ctx.http,
                MessageId::new(message_id),
                EditMessage::new().content(truncate_for_discord(content)),
            )
            .await
            .map(|_| ())
            .map_err(MessengerError::new)
    }

    async fn delete_reaction(
        &self,
        _room_id: u64,
        message_id: u64,
        user_id: u64,
        emoji: &str,
    ) -> MessengerResult<()> {
        let msg = self
            .channel_id
            .message(&self.ctx.http, MessageId::new(message_id))
            .await
            .map_err(MessengerError::new)?;
        msg.delete_reaction(
            &self.ctx.http,
            Some(UserId::new(user_id)),
            ReactionType::Unicode(emoji.to_string()),
        )
        .await
        .map(|_| ())
        .map_err(MessengerError::new)
    }

    async fn reply_ephemeral(&self, content: String) -> MessengerResult<()> {
        let content = truncate_for_discord(content);

        match &self.interaction {
            Some(interaction) => {
                // 初回応答を試み、失敗したら followup で送る
                let response = CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(content.clone())
                        .ephemeral(true),
                );
                if interaction
                    .create_response(&self.ctx.http, response)
                    .await
                    .is_err()
                {
                    let followup = CreateInteractionResponseFollowup::new()
                        .content(content)
                        .ephemeral(true);
                    interaction
                        .create_followup(&self.ctx.http, followup)
                        .await
                        .map_err(MessengerError::new)?;
                }
                Ok(())
            }
            None => {
                // interactionを持たない文脈では通常メッセージとしてフォールバック
                self.send(0, content).await.map(|_| ())
            }
        }
    }
}

/// Serenity イベントハンドラ
pub struct Handler {
    /// ボット本体
    pub bot: Bot,
    /// プロセス内でslashコマンドを登録済みのGuild(ID)集合
    pub registered_guilds: Arc<Mutex<HashSet<u64>>>,
}

impl Handler {
    pub fn new(bot: Bot) -> Self {
        Self {
            bot,
            registered_guilds: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// slashコマンドをGuildに登録する(プロセス内で1回のみ)
    async fn ensure_commands_registered(&self, ctx: &Context, guild_id: GuildId) {
        // ロックをawait越えさせないため先に判定だけ済ませる
        let should_register = {
            let registered = self.registered_guilds.lock().expect("mutex poisoned");
            !registered.contains(&guild_id.get())
        };
        if !should_register {
            return;
        }

        let commands = vec![command_vote(), command_find(), command_skip()];
        if let Err(e) = guild_id.set_commands(&ctx.http, commands).await {
            eprintln!("slashコマンド登録に失敗 (guild: {}): {e}", guild_id.get());
        } else {
            // 登録成功時のみ「登録済み」とマークする(失敗時は再試行可能)
            self.registered_guilds
                .lock()
                .expect("mutex poisoned")
                .insert(guild_id.get());
        }
    }

    /// ルームの表示名(サーバー名/チャンネル名)をキャッシュから取得する
    ///
    /// 名前が解決できない(ネットワークエラー等)場合は None を返す。
    async fn snapshot_room_name(&self, ctx: &Context, room_id: u64) -> Option<String> {
        // Cache::channel は非推奨のため、ギルドごとにチャンネルを走査して探す
        let channel_id = ChannelId::new(room_id);
        for guild_id in ctx.cache.guilds() {
            let Some(guild) = ctx.cache.guild(guild_id) else {
                continue;
            };
            if let Some(channel) = guild.channels.get(&channel_id) {
                return Some(format!("{}/{}", guild.name, channel.name));
            }
        }
        None
    }

    /// 名前がわかるルームについて、表示名をキャッシュから更新する
    ///
    /// 未登録のルームは作らない。
    async fn refresh_room_names(&self, ctx: &Context) {
        for room_id in self.bot.manager.room_ids().await {
            if let Some(name) = self.snapshot_room_name(ctx, room_id).await {
                self.bot.manager.set_room_name(room_id, name).await;
            }
        }
    }

    /// リアクションイベント(add/remove共通)
    async fn handle_reaction(&self, ctx: Context, reaction: Reaction, added: bool) {
        // ボット自身のリアクションは無視
        let Some(user_id) = reaction.user_id else {
            return;
        };
        let emoji = match &reaction.emoji {
            ReactionType::Unicode(emoji) if emoji == "👍" => "👍",
            ReactionType::Unicode(emoji) if emoji == "👎" => "👎",
            _ => return,
        };

        let room_id = reaction.channel_id.get();
        if !self.bot.manager.has_room(room_id).await {
            return; // 未登録チャンネルでは何もしない
        }

        let msgr = DiscordMessenger::channel(&ctx, room_id);
        engine::reaction_changed(
            &self.bot,
            &msgr,
            room_id,
            reaction.message_id.get(),
            user_id.get(),
            emoji,
            added,
        )
        .await;
    }

    /// メンバーコマンド(!queue等)の実行権限チェック
    ///
    /// キュー参加者(またはキュー未定義時の誰でも)に許可する
    async fn can_manage(&self, room_id: u64, user_id: u64) -> bool {
        engine::can_manage(&self.bot.manager, room_id, user_id).await
    }

    /// メンバーコマンド実行順の拒否メッセージ
    async fn deny_manage(&self, msgr: &SharedMessenger) {
        let _ = msgr
            .reply_ephemeral("この操作はキューに参加しているユーザーのみ実行できます。".to_string())
            .await;
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return; // ボット自身は無視
        }

        let room_id = msg.channel_id.get();
        let user_id = msg.author.id.get();

        // ボットへのメンション → チャンネル登録 + slashコマンド登録
        if msg.mentions_me(&ctx).await.unwrap_or(false) {
            let fresh = engine::register_room(&self.bot, room_id).await;
            // 表示名(サーバー名/チャンネル名)をキャッシュから記録
            if let Some(name) = self.snapshot_room_name(&ctx, room_id).await {
                self.bot.manager.set_room_name(room_id, name).await;
            }
            if let Some(guild_id) = msg.guild_id {
                self.ensure_commands_registered(&ctx, guild_id).await;
            }
            let msgr = DiscordMessenger::channel(&ctx, room_id);
            let text = m::register_message(fresh, self.bot.web_url.as_deref());
            let _ = msgr.send(room_id, text).await;
            return;
        }

        // 未登録チャンネルでは何もしない(部屋を作らない)
        if !self.bot.manager.has_room(room_id).await {
            return;
        }

        let manager = &self.bot.manager;
        let content = msg.content.trim();
        let msgr = DiscordMessenger::channel(&ctx, room_id);

        // 英単語1語 → 回答として受理
        if REGEX_WORD.is_match(content) {
            engine::try_word(&self.bot, &msgr, room_id, user_id, content).await;
            return;
        }

        // ?単語 → あいまい+部分一致の簡易検索
        if let Some(query) = content.strip_prefix('?') {
            let query = query.trim();
            if !query.is_empty() {
                engine::find_words_combined(&msgr, manager, room_id, query, false).await;
            }
            return;
        }

        // !queue @user... → 順番設定
        if let Some(rest) = content.strip_prefix("!queue") {
            let users = parse_mentions(rest);
            if !self.can_manage(room_id, user_id).await {
                self.deny_manage(&msgr).await;
                return;
            }
            engine::set_queue(&msgr, manager, room_id, users).await;
            return;
        }

        // !addqueue @user... → 参加
        if let Some(rest) = content.strip_prefix("!addqueue") {
            let users = parse_mentions(rest);
            if !self.can_manage(room_id, user_id).await {
                self.deny_manage(&msgr).await;
                return;
            }
            for u in users {
                engine::add_queue(&msgr, manager, room_id, u).await;
            }
            return;
        }

        // !addwords w1, w2 ... → 辞書一括追加
        if let Some(rest) = content
            .strip_prefix('!')
            .and_then(|s| s.strip_prefix("addwords"))
        {
            let words = parse_word_list(rest);
            if !self.can_manage(room_id, user_id).await {
                self.deny_manage(&msgr).await;
                return;
            }
            engine::add_words(&msgr, manager, room_id, words).await;
            return;
        }

        // !skip [n] → スキップ(引数なしは1)
        if let Some(rest) = content
            .strip_prefix('!')
            .and_then(|s| s.strip_prefix("skip"))
        {
            let len = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            if !self.can_manage(room_id, user_id).await {
                self.deny_manage(&msgr).await;
                return;
            }
            engine::skip(&msgr, manager, room_id, len).await;
            return;
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected! (v4)", ready.user.name);
        // 登録済みルームの表示名をキャッシュから更新(v4以前のデータや
        // 再起動前の名前変更を反映・接続直後のためキャッシュが不完全でも問題ない)
        self.refresh_room_names(&ctx).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };

        let room_id = command.channel_id.get();
        let user_id = command.user.id.get();
        let msgr = DiscordMessenger::from_interaction(&ctx, &command);

        if !self.bot.manager.has_room(room_id).await {
            let _ = msgr
                .reply_ephemeral(
                    "このチャンネルはしりとりチャンネルとして登録されていません。ボットにメンションして登録してください。"
                        .to_string(),
                )
                .await;
            return;
        }

        match command.data.name.as_str() {
            "vote" => {
                let good = command
                    .data
                    .options
                    .first()
                    .and_then(|opt| opt.value.as_i64())
                    .map(|v| v != 0)
                    .unwrap_or(true);

                if !self.can_manage(room_id, user_id).await {
                    self.deny_manage(&msgr).await;
                    return;
                }

                engine::apply_vote(&self.bot, &msgr, room_id, user_id, good, false).await;
                let _ = msgr
                    .reply_ephemeral(format!(
                        "{} に投票しました。",
                        if good { "有効" } else { "無効" }
                    ))
                    .await;
            }
            "find" => {
                let word = command
                    .data
                    .options
                    .first()
                    .and_then(|opt| opt.value.as_str())
                    .unwrap_or_default()
                    .to_string();

                let mode_opt = command
                    .data
                    .options
                    .get(1)
                    .and_then(|opt| opt.value.as_i64());
                let max_dist_opt = command
                    .data
                    .options
                    .get(2)
                    .and_then(|opt| opt.value.as_i64())
                    .map(|v| v as usize);

                let (mode, max_dist) = match mode_opt {
                    Some(0) => (SearchMode::Exact, None),
                    Some(1) => (SearchMode::Substring, None),
                    Some(2) => (SearchMode::Prefix, None),
                    Some(3) => (SearchMode::Suffix, None),
                    Some(4) | None => (
                        SearchMode::Fuzzy,
                        Some(max_dist_opt.unwrap_or_else(|| word.chars().count() / 4)),
                    ),
                    _ => (SearchMode::Exact, None),
                };

                if word.is_empty() {
                    let _ = msgr
                        .reply_ephemeral("検索する単語を指定してください。".to_string())
                        .await;
                    return;
                }

                engine::find_words(
                    &msgr,
                    &self.bot.manager,
                    room_id,
                    &word,
                    mode,
                    max_dist,
                    true,
                )
                .await;
            }
            "skip" => {
                let len = command
                    .data
                    .options
                    .first()
                    .and_then(|opt| opt.value.as_i64())
                    .map(|n| n.max(1) as usize)
                    .unwrap_or(1);

                if !self.can_manage(room_id, user_id).await {
                    self.deny_manage(&msgr).await;
                    return;
                }

                engine::skip(&msgr, &self.bot.manager, room_id, len).await;
            }
            _ => {}
        }
    }

    async fn reaction_add(&self, ctx: Context, reaction: Reaction) {
        self.handle_reaction(ctx, reaction, true).await;
    }

    async fn reaction_remove(&self, ctx: Context, reaction: Reaction) {
        self.handle_reaction(ctx, reaction, false).await;
    }
}

/// "<@123>" / "<@!123>" を解析してユーザーIDリストにする
fn parse_mentions(text: &str) -> Vec<u64> {
    REGEX_MENTION
        .captures_iter(text)
        .filter_map(|caps| caps.get(1))
        .filter_map(|m| m.as_str().parse::<u64>().ok())
        .collect()
}

/// カンマ/空白区切りの英単語リストを正規化して収集
fn parse_word_list(text: &str) -> Vec<String> {
    text.split([',', ' '])
        .map(str::trim)
        .filter(|w| REGEX_WORD.is_match(w))
        .map(|w| w.to_lowercase())
        .collect()
}

/// /vote コマンド定義
pub fn command_vote() -> CreateCommand {
    CreateCommand::new("vote")
        .description("最新の回答に投票します。")
        .add_option(
            CreateCommandOption::new(
                serenity::all::CommandOptionType::Integer,
                "vote",
                "有効/無効(過半数で決着)",
            )
            .required(true)
            .add_int_choice("有効", 1)
            .add_int_choice("無効", 0),
        )
}

/// /find コマンド定義
pub fn command_find() -> CreateCommand {
    CreateCommand::new("find")
        .description("辞書の単語を検索します。")
        .add_option(
            CreateCommandOption::new(
                serenity::all::CommandOptionType::String,
                "word",
                "既出の類似単語を検索します。",
            )
            .required(true),
        )
        .add_option(
            CreateCommandOption::new(
                serenity::all::CommandOptionType::Integer,
                "mode",
                "検索形式(オプション / 既定はあいまい検索)",
            )
            .required(false)
            .add_int_choice("完全一致", 0)
            .add_int_choice("部分一致", 1)
            .add_int_choice("前方一致", 2)
            .add_int_choice("後方一致", 3)
            .add_int_choice("あいまい", 4),
        )
        .add_option(
            CreateCommandOption::new(
                serenity::all::CommandOptionType::Integer,
                "max_dist",
                "あいまい検索の最大距離(既定は文字数の25%)",
            )
            .required(false),
        )
}

/// /skip コマンド定義
pub fn command_skip() -> CreateCommand {
    CreateCommand::new("skip")
        .description("指定した人数のユーザーをスキップします。")
        .add_option(
            CreateCommandOption::new(
                serenity::all::CommandOptionType::Integer,
                "count",
                "スキップする人数(既定は1)",
            )
            .required(false),
        )
}

#[cfg(test)]
mod tests {
    use super::{parse_mentions, parse_word_list, truncate_for_discord};

    #[test]
    fn parse_mentions_extracts_ids() {
        let ids = parse_mentions("<@123> <@!456> <@789>");
        assert_eq!(ids, vec![123, 456, 789]);
        assert!(parse_mentions("no mention here").is_empty());
    }

    #[test]
    fn parse_word_list_normalizes() {
        let words = parse_word_list("Apple, GRAPE, banana");
        assert_eq!(words, vec!["apple", "grape", "banana"]);
        // 不正要素は除外される
        // 分割は空白でも行われるため各トークンごとに妥当性判定される
        let words = parse_word_list("ok, あいう, with space, valid-word");
        assert_eq!(words, vec!["ok", "with", "space", "valid-word"]);
    }

    #[test]
    fn truncate_long_text() {
        let long = "a".repeat(5000);
        let cut = truncate_for_discord(long);
        assert!(cut.chars().count() <= 2000);
        assert!(cut.ends_with("...(以降省略)"));
        // 短文はそのまま
        assert_eq!(truncate_for_discord("abc".to_string()), "abc");
    }
}
