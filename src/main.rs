use std::sync::{atomic::AtomicBool, Arc};

use anyhow::Result;
use signal_hook::{consts::SIGTERM, flag, low_level::exit};

use crate::{bot::{config::BotConfig}};

mod bot;
mod database;
mod macros;

#[tokio::main]
async fn main() -> Result<()> {
    let term_flag = Arc::new(AtomicBool::new(false));
    flag::register_conditional_default(SIGTERM, Arc::clone(&term_flag)).unwrap();

    let config = match BotConfig::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Failed to get config from environment\n{:?}", e);
            exit(1);
        }
    };
    
    loop {

    }
    
    Ok(())
}

fn safe_shutdown() {

}