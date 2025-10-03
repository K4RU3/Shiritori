use serenity::all::{CommandInteraction, CreateCommand, CreateCommandOption};

use crate::{actions::{find_words, skip, BotContext, SearchOptions}, room::RoomManager, MatchMode};

pub fn command() -> CreateCommand {
    CreateCommand::new("skip")
        .description("指定した人数ユーザーをスキップします。")
        .add_option(
            CreateCommandOption::new(serenity::all::CommandOptionType::Integer, "スキップする人数", "スキップする人数を指定します。")
                .required(false)
        )
}

pub async fn run(ctx: &BotContext, manager: &RoomManager, command: &CommandInteraction) {
    // オプションからスキップ人数を取得（未指定なら1）
    let skip_count: usize = command
        .data
        .options
        .get(0)
        .and_then(|opt| Some(opt.value.clone()))
        .and_then(|v| v.as_i64())
        .map(|n| n.max(1) as usize) // 1未満なら1に調整
        .unwrap_or(1);

    // 既存の skip 関数を呼び出す
    skip(ctx, manager, skip_count).await;
}