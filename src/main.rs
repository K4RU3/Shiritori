use std::{sync::Arc, time::Duration};

use chrono::Local;
use serenity::{all::GatewayIntents, Client};
use shiritori_v3::{arc_rwlock, bot_handler::Handler, room::RoomManager};
use tokio::{signal, sync::RwLock, time};

// 終了用関数
async fn shutdown(manager: Arc<RwLock<RoomManager>>, room_path: &str, word_path: &str) {
    println!("終了処理開始...");
    let _ = manager.write().await.save_all(room_path, word_path);
    println!("Bot終了");
    std::process::exit(0); // 明示的にプロセス終了
}

#[tokio::main]
async fn main() {
    // ボットのトークン（環境変数から取得も可）
    let token = std::env::var("DISCORD_BOT_KEY").expect("DISCORD_BOT_KEY not set");
    let room_path = std::env::var("ROOMS_PATH").expect("ROOMS_PATH not set. example: ./save/rooms.json");
    let word_path = std::env::var("WORDS_PATH").expect("WORDS_PATH not set. example: ./save/words/");
    let manager = arc_rwlock!(RoomManager::load_or_new(&room_path).await);

    // ハンドラ作成
    let handler = Handler {
        manager: manager.clone(),
        room_path: room_path.to_string(),
        word_path: word_path.to_string(),
    };

    // ボット情報
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGE_REACTIONS;

    // クライアント作成
    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .expect("Err creating client");

    println!("Bot is running...");

    // =========================
    // 定期保存タスク
    // =========================
    {
        let manager_clone = Arc::clone(&manager);
        let room_path_clone = room_path.to_string();
        let word_path_clone = word_path.to_string();

        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(86400)); // 1日ごと
            loop {
                interval.tick().await;
                let _ = manager_clone.write().await.save_all(&room_path_clone, &word_path_clone);
                let now = Local::now();
                println!("[{}] 定期保存完了", now.format("%Y-%m-%d %H:%M:%S"));
            }
        });
    }

    // =========================
    // シグナル監視タスク
    // Windows: Ctrl+C
    // Linux: SIGTERM (docker stop)
    // =========================
    {
        let manager_clone = Arc::clone(&manager);
        let room_path_clone = room_path.to_string();
        let word_path_clone = word_path.to_string();

        // Windows Ctrl+C
        #[cfg(windows)]
        tokio::spawn(async move {
            signal::ctrl_c().await.expect("Failed to listen for ctrl_c");
            shutdown(manager_clone, &room_path_clone, &word_path_clone).await;
        });

        // Linux SIGTERM
        #[cfg(unix)]
        tokio::spawn(async move {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to listen for SIGTERM");
            term.recv().await;
            shutdown(manager_clone, &room_path_clone, &word_path_clone).await;
        });
    }

    // ボット起動
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}