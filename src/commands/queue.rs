use serenity::all::{CommandInteraction, CreateCommand, CreateCommandOption};

use crate::{actions::{BotContext, vote}, room::RoomManager};

pub fn command() -> CreateCommand {
    CreateCommand::new("queue")
        .description("回答順を設定します。")
}

pub async fn run(ctx: &BotContext, manager: &RoomManager, command: &CommandInteraction) {
}
