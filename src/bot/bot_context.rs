use crate::{bot::config::BotConfig, database::repository::Repository};

use std::sync::Arc;

pub struct BotContext {
    pub config: Arc<BotConfig>,
    pub repo: Arc<Repository>,
}