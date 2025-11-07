use std::sync::Arc;

use serenity::{all::{Context, EventHandler, Ready}, async_trait};
use crate::bot::bot_context::BotContext;

#[allow(dead_code)]
pub struct Handler {
    pub ctx: Arc<BotContext>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _: Context, _ready: Ready) {
        println!("ready for handle");
    }
}