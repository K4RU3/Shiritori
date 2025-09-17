use std::sync::Arc;

use serenity::{
    all::{CommandInteraction, CreateInteractionResponse, CreateInteractionResponseMessage, EditMessage, Interaction, MessageId, Reaction},
    async_trait,
    model::{channel::Message, gateway::Ready},
    prelude::*,
};

use crate::{actions::*, commands::{self, message_commands}, room::RoomManager};

pub struct Handler {
    pub manager: Arc<RwLock<RoomManager>>,
    pub room_path: String,
    pub word_path: String,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        } // ボット自身は無視

        let bot_ctx = create_bot_context(&ctx, msg.channel_id.get(), &self.room_path, &self.word_path).await;

        let _ = message_commands::handle(ctx, msg, bot_ctx, self.manager.clone()).await;
    }

    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            let bot_ctx = create_bot_context_from_interaction(&ctx, &command, &self.room_path, &self.word_path).await;
            let manager = self.manager.read().await;

            match command.data.name.as_str() {
                "vote" => commands::vote::run(&bot_ctx, &manager, &command).await,
                "find" => commands::find::run(&bot_ctx, &manager, &command).await,
                _ => {}
            }
        }
    }

    async fn reaction_add(&self, ctx: Context, reaction: Reaction) {
        let channel_id = reaction.channel_id.get();
        let bot_ctx = create_bot_context(&ctx, channel_id, &self.room_path, &self.word_path).await;
        reaction_changed(&self, &ctx, &reaction, true, &bot_ctx).await;
    }

    async fn reaction_remove(&self, ctx: Context, reaction: Reaction) {
        let channel_id = reaction.channel_id.get();
        let bot_ctx = create_bot_context(&ctx, channel_id, &self.room_path, &self.word_path).await;
        reaction_changed(&self, &ctx, &reaction, false, &bot_ctx).await;
    }
}

/// BotContext を生成
async fn create_bot_context(ctx: &Context, channel_id: u64, room_path: &str, word_path: &str) -> BotContext {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    let channel = serenity::model::id::ChannelId::new(channel_id);

    let send_fn = {
        let ctx = ctx.clone();
        Arc::new(move |content: String| {
            let ctx = ctx.clone();
            Box::pin(async move {
                let sent_msg = channel.say(&ctx.http, content).await;
                match sent_msg {
                    Ok(m) => m.id.get(),
                    Err(_) => 0,
                }
            }) as Pin<Box<dyn Future<Output = u64> + Send>>
        })
    };

    let edit_fn = {
        let ctx = ctx.clone();
        Arc::new(move |msg_id: u64, content: String| {
            let ctx = ctx.clone();
            Box::pin(async move {
                let message_id = MessageId::from(msg_id);

                // EditMessage はメソッドチェーンで消費されるので builder を直接渡す
                let builder = EditMessage::default().content(content);

                let _ = channel.edit_message(&ctx.http, message_id, builder).await;
            }) as Pin<Box<dyn Future<Output = ()> + Send>>
        })
    };

    let response_fn = Arc::new(|_: String| Box::pin(async move {}) as Pin<Box<dyn Future<Output = ()> + Send>>);

    BotContext {
        room_id: channel_id,
        room_path: room_path.to_string(),
        word_path: word_path.to_string(),
        send: send_fn,
        edit: edit_fn,
        delete: Arc::new(|_| Box::pin(async move {})),
        response: response_fn,
    }
}

/// Interaction 用に response だけ差し替えるヘルパー
pub async fn create_bot_context_from_interaction(
    ctx: &Context,
    interaction: &CommandInteraction,
    room_path: &str,
    word_path: &str
) -> BotContext {
    use std::pin::Pin;

    // まず通常の channel_id 用 BotContext を作る
    let mut bot_ctx = create_bot_context(ctx, interaction.channel_id.get(), room_path, word_path).await;

    // response だけ上書き
    let ctx = ctx.clone();
    let interaction = interaction.clone();
    bot_ctx.response = Arc::new(move |content: String| {
        let ctx = ctx.clone();
        let interaction = interaction.clone();
        Box::pin(async move {
            // 最初の返信がまだなら create_interaction_response、すでに返信済みなら followup
            let _ = interaction.create_response(&ctx.http, 
                CreateInteractionResponse::Message(CreateInteractionResponseMessage::new()
                    .content(content)
                    .ephemeral(true)
            )).await;
        }) as Pin<Box<dyn Future<Output = ()> + Send>>
    });

    bot_ctx
}
