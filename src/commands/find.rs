use serenity::all::{CommandInteraction, CreateCommand, CreateCommandOption};

use crate::{actions::{find_words, BotContext, SearchOptions}, room::RoomManager, MatchMode};

pub fn command() -> CreateCommand {
    CreateCommand::new("find")
        .description("単語を検索します。")
        .add_option(
            CreateCommandOption::new(serenity::all::CommandOptionType::String, "検索する単語", "既出の類似単語を検索します。")
                .required(true)
        )
        .add_option(
            CreateCommandOption::new(serenity::all::CommandOptionType::Integer, "検証タイプ", "検索する形式を選択(オプション)")
                .required(false)
                .add_int_choice("完全一致", 0)
                .add_int_choice("部分一致", 1)
                .add_int_choice("前方一致", 2)
                .add_int_choice("後方一致", 3)
                .add_int_choice("レーヴェンシュタイン距離", 4)
        )
        .add_option(
            CreateCommandOption::new(serenity::all::CommandOptionType::Integer, "最大距離", "検索するレーヴェーンシュタイン距離の最大値(デフォルトで25%の文字)")
                .required(false)
        )
}

pub async fn run(ctx: &BotContext, manager: &RoomManager, command: &CommandInteraction) {
    let data = &command.data;

    // 検索する単語（必須）
    let word = if let Some(first) = data.options.first() {
        first.value.as_str().unwrap_or_default()
    } else {
        ""
    };

    // 検証タイプ（オプション）
    let match_mode_option = data.options.get(1).and_then(|opt| opt.value.as_i64());

    let match_mode = match match_mode_option {
        Some(0) => Some(MatchMode::Exact),
        Some(1) => Some(MatchMode::Substring),
        Some(2) => Some(MatchMode::Prefix),
        Some(3) => Some(MatchMode::Suffix),
        Some(4) => None, // レーヴェンシュタイン距離は fuzzy_distance に対応
        _ => None
    };

    // 最大距離（オプション）
    let fuzzy_distance = data.options.get(2).and_then(|opt| opt.value.as_i64()).map(|v| v as usize);

    let options = SearchOptions {
        fuzzy_distance: if match_mode_option == Some(4) { fuzzy_distance.or(Some(word.len() / 4)) } else { None },
        match_mode,
    };

    let _ = find_words(&ctx, &manager, word, options, false).await;
    println!("command find");
}