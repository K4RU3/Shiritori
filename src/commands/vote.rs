use serenity::all::{CommandInteraction, CreateCommand, CreateCommandOption};

use crate::{actions::{BotContext, vote}, room::RoomManager};

pub fn command() -> CreateCommand {
    CreateCommand::new("vote")
        .description("最新の回答に投票します。")
        .add_option(
            CreateCommandOption::new(
                serenity::all::CommandOptionType::Integer,
                "投票",
                "過半数で決着",
            )
            .required(true)
            .add_int_choice("有効", 1)
            .add_int_choice("無効", 0),
        )
}

pub async fn run(ctx: &BotContext, manager: &RoomManager, command: &CommandInteraction) {
    // コマンド実行者の ID
    let user_id = command.user.id.get();

    // 投票オプションを bool に変換（1 = true, 0 = false）
    let Some(first) = command.data.options.first() else { return; };
    let Some(value) = first.value.as_i64() else { return; };
    let vote_value = value != 0;

    // 既存の vote 関数を呼び出す
    vote(ctx, manager, user_id, vote_value, false).await;
}
