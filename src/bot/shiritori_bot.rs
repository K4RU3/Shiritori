use std::sync::Arc;

use serenity::{Client};

use crate::{bot::{bot_context::BotContext, config::BotConfig}, database::repository::Repository};

pub struct Bot {
    client: Client,
    ctx: Arc<BotContext>
}

impl Bot {
    pub async fn new(config: BotConfig) -> anyhow::Result<Self> {
        let repository = Repository::new(&config.db_path())?;

        let ctx = BotContext {
            config: Arc::new(config),
            repo: Arc::new(repository)
        };


    }

    fn shutdown() {}
}