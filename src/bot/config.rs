use serenity::all::GatewayIntents;

#[derive(Debug, Clone)]
pub struct BotConfig {
    token: String,
    gateway_intents: GatewayIntents,
    db_path: String,
}

impl BotConfig {
    pub fn new(token: String, db_path: String, gateway_intents: GatewayIntents) -> Self {
        Self {
            token, db_path, gateway_intents
        }
    }

    pub fn token(&self) -> String {
        self.token.clone()
    }

    pub fn gateway_intents(&self) -> GatewayIntents {
        self.gateway_intents.clone()
    }

    pub fn db_path(&self) -> String {
        self.db_path.clone()
    }
}