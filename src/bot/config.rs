#![allow(dead_code)]
use serenity::all::GatewayIntents;

#[derive(Debug, Clone)]
pub struct BotConfig {
    token: String,
    gateway_intents: GatewayIntents,
    db_path: String,
    init_sql_path: String,
}

impl BotConfig {
    pub fn new(token: String, db_path: String, gateway_intents: GatewayIntents, init_sql_path: String) -> Self {
        Self {
            token, db_path, gateway_intents, init_sql_path
        }
    }
    
    pub fn from_env() -> Result<Self, std::env::VarError> {
        const ENV_TOKEN: &str = "BOT_TOKEN";
        const ENV_DBPATH: &str = "DB_PATH";
        const ENV_INIT_SQL_PATH: &str = "INIT_SQL";
        dotenv::dotenv().ok();
        let token = std::env::var(ENV_TOKEN)?;
        let db_path = std::env::var(ENV_DBPATH)?;
        let init_sql_path = std::env::var(ENV_INIT_SQL_PATH)?;
        let gateway_intents = GatewayIntents::privileged();
        
        Ok(
            Self {
                token,
                db_path,
                init_sql_path,
                gateway_intents
            }
        )
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
    
    pub fn init_sql_path(&self) -> String {
        self.init_sql_path.clone()
    }
}