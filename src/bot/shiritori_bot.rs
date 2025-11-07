use std::{fs::{self}, sync::Arc};

use anyhow::Result;
use serenity::{Client};

use crate::{bot::{bot_context::BotContext, config::BotConfig, handler::Handler}, database::{db::DataBase, repository::Repository}};

#[allow(dead_code)]
pub struct Bot {
    client: Arc<Client>,
    ctx: Arc<BotContext>
}

impl Bot {
    pub async fn new(config: BotConfig) -> Result<Self> {
        let db_path = &config.db_path();
        let init_sql = &fs::read_to_string(config.init_sql_path())?;
        let db = DataBase::new(db_path, Some(init_sql)).await?;
        let repository = Repository::new(db)?;

        let ctx = BotContext {
            config: Arc::new(config.clone()),
            repo: Arc::new(repository)
        };
        let arc_ctx = Arc::new(ctx);
        
        let handler = Handler { ctx: arc_ctx.clone() };
        
        let mut clinet = Client::builder(config.token(), config.gateway_intents())
            .event_handler(handler)
            .await?;
        clinet.start().await?;
        let arc_client = Arc::new(clinet);

        Ok(
            Self {
                ctx: arc_ctx.clone(),
                client: arc_client.clone()
            }
        )
    }

    fn shutdown(&self) {
        //TODO: 安全な処理を今後実装
    }
}