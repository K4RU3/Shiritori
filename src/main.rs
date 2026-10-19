use std::sync::Arc;

use dotenv::dotenv;
use serenity::{Client, all::GatewayIntents};
use shiritori_v4::{
    RoomManager,
    bot::{Bot, handler::Handler},
    mean::NoopMeaningProvider,
    web::WebConfig,
};
use tokio::{signal, time};

/// 定期保存間隔(秒)
const SAVE_INTERVAL_SECS: u64 = 60;

/// データファイルのパス(DATA_DIR/data.json)
fn data_path() -> String {
    let dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_string());
    let _ = std::fs::create_dir_all(&dir);
    format!("{dir}/data.json")
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    let token = std::env::var("DISCORD_BOT_TOKEN").expect("DISCORD_BOT_TOKEN is not set");
    let path = data_path();

    let manager = match RoomManager::load_file(&path).await {
        Ok(manager) => manager,
        Err(e) => {
            eprintln!(
                "データファイルの読み込みに失敗({path}): {e}\n{path}.corrupt / {path}.bak を確認してください。空の状態で起動します。"
            );
            RoomManager::new()
        }
    };

    // Web機能は3つの環境変数がそろった場合のみ有効
    let web_config = WebConfig::from_parts(
        std::env::var("WEB_BIND").ok(),
        std::env::var("WEB_PUBLIC_URL").ok(),
        std::env::var("WEB_PASSWORD").ok(),
    );

    // 公開URLは辞書検索ページへのリンクに利用(Web有効時のみ)
    let web_url = web_config.as_ref().map(|c| c.public_url.clone());
    let bot = Bot::new(manager.clone(), Arc::new(NoopMeaningProvider), web_url);
    // ルーム名の更新要求チャネル: Web側(インポート完了)から Handler へ通知する
    let name_refresh = Arc::new(tokio::sync::Notify::new());
    let handler = Handler::new(bot.clone()).with_name_refresh(name_refresh.clone());

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGE_REACTIONS;

    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .expect("Err creating client");

    println!("Bot is running... (v4)");

    // Webサーバー起動(失敗してもプロセスは継続)
    if let Some(config) = web_config {
        let web_state =
            shiritori_v4::web::AppState::new(config.clone(), manager.clone(), path.clone())
                .with_name_refresh(name_refresh.clone());
        match shiritori_v4::web::serve(web_state).await {
            Ok(addr) => println!("Webサーバー起動: {addr}"),
            Err(e) => eprintln!("Webサーバー起動に失敗(無視して継続): {e}"),
        }
    } else {
        println!("Web機能は無効です(WEB_BIND/WEB_PUBLIC_URL/WEB_PASSWORD の3つが必要)");
    }

    // 定期保存タスク
    {
        let manager = manager.clone();
        let path = path.clone();
        tokio::spawn(async move {
            let mut interval = time::interval(std::time::Duration::from_secs(SAVE_INTERVAL_SECS));
            loop {
                interval.tick().await;
                if let Err(e) = manager.save_file(&path).await {
                    eprintln!("定期保存に失敗: {e}");
                } else {
                    println!("[保存] {}", chrono::Local::now().format("%H:%M:%S"));
                }
            }
        });
    }

    // シグナル監視(Linux: SIGTERM / Windows: Ctrl+C。LinuxはSIGINTも捕捉)
    {
        let manager = manager.clone();
        let path = path.clone();
        tokio::spawn(async move {
            #[cfg(windows)]
            signal::ctrl_c().await.expect("ctrl_c listener failed");
            #[cfg(unix)]
            {
                let mut term = signal::unix::signal(signal::unix::SignalKind::terminate())
                    .expect("SIGTERM listener failed");
                tokio::select! {
                    _ = term.recv() => {},
                    _ = signal::ctrl_c() => {},
                }
            }
            println!("終了処理開始...");
            if let Err(e) = manager.save_file(&path).await {
                eprintln!("終了時保存に失敗: {e}");
            }
            println!("Bot終了");
            std::process::exit(0);
        });
    }

    if let Err(why) = client.start().await {
        eprintln!("Client error: {why:?}");
    }
}
