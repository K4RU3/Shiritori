use std::{collections::HashSet, pin::Pin, sync::Arc};

use serenity::all::{ChannelId, Context, Reaction, ReactionType};

use crate::{
    arc_rwlock, bot_handler::Handler, message::{
        generate_add_queue_message, generate_find_message, generate_set_queue_message, TryMessageBuilder
    }, room::{RoomManager, VoteState}, util::get_word_mean_jp, MatchMode, SharedFuzzyIndex
};

#[derive(Clone)]
pub struct BotContext {
    pub room_id: u64,
    pub room_path: String,
    pub word_path: String,
    pub send: Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = u64> + Send>> + Send + Sync>,
    pub edit: Arc<dyn Fn(u64, String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>,
    pub delete: Arc<dyn Fn(u64) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>,
    pub response: Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>,
}

pub async fn try_word(ctx: &BotContext, manager: &RoomManager, user_id: u64, word: &str, _message_id: u64) {
    let Some(room_arc) = manager.get_room_mut(ctx.room_id).await else {
        return; // Noneであれば何もしない
    };

    // 投票基本情報
    {
        let mut room = room_arc.write().await;

        room.vote_state = VoteState {
            target_user: Some(user_id),
            target_word: Some(word.to_string()),
            vote_message: None,
            good_users: HashSet::new(),
            bad_users: HashSet::new(),
            message_builder: arc_rwlock!(TryMessageBuilder::init(user_id, word.to_string(), room.user_queue.clone())),
        };
    }


    // メッセージ生成
    let message = {
        let room = room_arc.read().await;
        room.vote_state.message_builder.read().await.build()
    };

    let vote_message_id = (ctx.send)(message).await;

    // 投票メッセージ設定
    {
        let mut room = room_arc.write().await;
        room.vote_state.vote_message = Some(vote_message_id);
    }

    // 非同期でメッセージ更新
    let _ = tokio::join!(
        // 類似単語検索
        async {
            let index = {
                let mut room = room_arc.write().await;
                let mut index = room.index.clone();
                if index.is_none() {
                    room.load_words(&ctx.word_path).await;
                    index = room.index.clone();
                }

                if index.is_none() { return };

                index.unwrap()
            };

            let mearged_words = default_find(&index, word.to_string()).await;

            // ほかの変更と同期させるため、ロックを保持したまま編集
            {
                let room = room_arc.write().await;
                let mut builder = room.vote_state.message_builder.write().await;
                builder.like_words = Some(mearged_words);

                if room.vote_state.target_word.as_deref() != Some(word) { return }

                let message = builder.build();
                (ctx.edit)(vote_message_id, message).await;
            }
        },

        // 意味検索
        async {
            let mean = get_word_mean_jp(word.to_string()).await;

            // ほかの変更と同期させるため、ロックを保持したまま編集
            {
                let room = room_arc.write().await;
                let mut builder = room.vote_state.message_builder.write().await;
                builder.mean = Some(mean);

                if room.vote_state.target_word.as_deref() != Some(word) { return }

                let message = builder.build();
                (ctx.edit)(vote_message_id, message).await;
            }
        }
    );
}

pub async fn vote(
    ctx: &BotContext,
    manager: &RoomManager,
    user_id: u64,
    good: bool,
    cancel: bool,
) {
    // 1. 対象の room を取得
    let Some(room_arc) = manager.get_room_mut(ctx.room_id).await else {
        return; // room がなければ終了
    };

    // 2. vote_state のチェック
    let (message_id, word) = {
        let room = room_arc.read().await;
        let vote_state = &room.vote_state;

        let mid = if let Some(m) = vote_state.vote_message { m } else { return; };
        let w = if let Some(w) = &vote_state.target_word { w.clone() } else { return; };

        (mid, w)
    };

    // 3. vote_state の更新
    {
        let mut room = room_arc.write().await;
        let vote_state = &mut room.vote_state;

        if cancel {
            // 投票キャンセル → 両方から削除
            vote_state.good_users.remove(&user_id);
            vote_state.bad_users.remove(&user_id);
        } else {
            if good {
                vote_state.good_users.insert(user_id);
                vote_state.bad_users.remove(&user_id);
            } else {
                vote_state.bad_users.insert(user_id);
                vote_state.good_users.remove(&user_id);
            }
        }
    }

    // 4. 過半数判定
    let (total_users, good_count, bad_count) = {
        let room = room_arc.read().await; // read ロック開始

        (
            room.user_queue.len(),
            room.vote_state.good_users.len(),
            room.vote_state.bad_users.len(),
        )
    }; // ここで room のロックは解放される

    let majority = (total_users / 2) + 1;

    {
        let mut room = room_arc.write().await;

        if good_count >= majority {
            // 4a. 過半数を超えた場合 → 結果通知
            let mut message = format!("投票終了！結果: YES({}) / NO({})\n\"{}\"は可決されました。", good_count, bad_count, word);

            // 次のユーザーに遷移
            if room.user_queue.len() > 0 {
                room.user_queue.rotate_left(1);
                let next_user_id_string: String = room.user_queue[0].to_string();
                message.push_str(&format!("\n次は<@{}> の番です。", next_user_id_string));
            }

            let _ = (ctx.edit)(message_id, message).await;

            // vote_state を初期化
            room.vote_state = Default::default();

            // 単語をリストに追加
            let mut index = room.index.clone();
            if index.is_none() {
                room.load_words(&ctx.word_path).await;
                index = room.index.clone();
            }

            if let Some(index) = index {
                index.add_word(word.clone()).await;
            }
        } else if bad_count >= majority {
            // 4b. 過半数 NO
            let mut message = format!("投票終了！結果: NO({}) / YES({})\n\"{}\"は否決されました。", bad_count, good_count, word);

            if room.user_queue.len() > 0 {
                let current_user_id_string: String = room.user_queue[0].to_string();
                message.push_str(&format!("\n<@{}> は、別の回答をしてください。", current_user_id_string));
            }

            let _ = (ctx.edit)(message_id, message).await;

            // vote_state をリセット
            room.vote_state = Default::default();
        } else {
            // 4c. 過半数に達していない場合 → message_builder を更新して表示
            // message_builder の更新とメッセージ生成をまとめる
            let message = {
                let mut builder = room.vote_state.message_builder.write().await;

                // vote_state から builder にコピー
                builder.vote_good = room.vote_state.good_users.iter().copied().collect();
                builder.vote_bad  = room.vote_state.bad_users.iter().copied().collect();

                // 他に必要な更新もここで行える
                builder.build()
            };

            // Discord 上のメッセージを更新
            let _ = (ctx.edit)(message_id, message).await;
        }
    }

    (ctx.response)(format!("{} に投票しました。", if good { "有効" } else { "無効" })).await;
}

pub async fn set_queue(ctx: &BotContext, manager: &RoomManager, users: Vec<u64>) {
    // まず参照でメッセージ生成
    let message = generate_set_queue_message(&users);

    // room に所有権を移す
    if let Some(room_arc) = manager.get_room_mut(ctx.room_id).await {
        let mut room = room_arc.write().await;
        room.user_queue = users;
    }

    // 送信タスクをバックグラウンドで spawn
    let response_future = (ctx.send)(message);
    tokio::spawn(async move {
        let _ = response_future.await;
    });
}

pub async fn add_queue(ctx: &BotContext, manager: &RoomManager, user_id: u64) {
    // 対象の room を取得
    let Some(room_arc) = manager.get_room_mut(ctx.room_id).await else {
        return;
    };

    let mut room = room_arc.write().await;

    // queue に追加
    let old_queue = room.user_queue.clone(); // メッセージ生成用に古い queue を保持
    room.user_queue.push(user_id);

    // メッセージ生成
    let message = generate_add_queue_message(&old_queue, user_id);

    // 非同期で送信
    let response_future = (ctx.send)(message);
    tokio::spawn(async move {
        let _ = response_future.await;
    });
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub fuzzy_distance: Option<usize>,       // None なら fuzzy は行わない
    pub match_mode: Option<MatchMode>,       // None なら部分一致は行わない
}

pub async fn find_words(ctx: &BotContext, manager: &RoomManager, word: &str, options: SearchOptions, global_message: bool) {
    let Some(room_arc) = manager.get_room_mut(ctx.room_id).await else {
        return;
    };

    let mut room = room_arc.write().await;

    let index = if let Some(index) = &room.index {
        index.clone() // 既にロード済みならそのまま使用
    } else {
        room.load_words(&ctx.word_path).await; // 未ロードならロード
        room.index.as_ref().unwrap().clone() // ロード後に取得
    };

    // like_list と match_list は Vec<String>
    let like_list = if let Some(dist) = options.fuzzy_distance {
        index.search_fuzzy(word, dist).await
    } else {
        vec![]
    };

    let match_list = if let Some(mode) = options.match_mode {
        index.search_match(word, mode).await
    } else {
        vec![]
    };

    // HashSet に入れて重複排除
    let mut unique_words: HashSet<String> = HashSet::new();
    for w in like_list.into_iter().chain(match_list.into_iter()) {
        unique_words.insert(w);
    }

    // Vec に戻す場合（必要なら）
    let merged_words: Vec<String> = unique_words.into_iter().collect();

    let message = generate_find_message(word, &merged_words);

    tokio::spawn({
        let ctx = ctx.clone();
        async move {
            if global_message {
                let _ = (ctx.send)(message).await; // Output = ()
            } else {
                let _ = (ctx.response)(message).await; // Output = u64
            }
        }
    });
}

pub async fn reaction_changed(handler: &Handler, ctx: &Context, reaction: &Reaction, add: bool, bot_ctx: &BotContext) {
    // ボットのリアクションを無効化
    let user_id = if let Some(user) = reaction.user_id { user } else { return };
    if user_id == ctx.cache.current_user().id { return };

    // 絵文字が 👍 / 👎 以外ならスキップ
    let is_good = match &reaction.emoji {
        ReactionType::Unicode(emoji) if emoji == "👍" => true,
        ReactionType::Unicode(emoji) if emoji == "👎" => false,
        _ => { return },
    };

    // 最新の投票以外スキップ
    let is_latest_vote = {
        let room_lock = handler.manager.read().await;
        match room_lock.get_room(reaction.channel_id.get()).await {
            Some(room) => room.vote_state.vote_message.unwrap_or(0) == reaction.message_id.get(),
            None => false,
        }
    };

    if !is_latest_vote {
        return;
    }

    if add {
        // 排他制御: 👍 なら 👎 を削除、👎 なら 👍 を削除
        if let Ok(msg) = reaction.message(&ctx.http).await {
            let opposite = if is_good { "👎" } else { "👍" };
            if let Err(why) = msg
                .delete_reaction(&ctx.http, Some(user_id), ReactionType::Unicode(opposite.to_string()))
                .await
            {
                eprintln!("排他リアクション削除に失敗: {:?}", why);
            }
        }
    }

    let manager = handler.manager.read().await;

    if add {
        vote(&bot_ctx, &manager, user_id.get(), is_good, false).await;
    } else {
        vote(&bot_ctx, &manager, user_id.get(), is_good, true).await;
    }
}

pub async fn default_find(index: &SharedFuzzyIndex, query: String) -> Vec<String> {
    // 並列実行
    let (like_word, match_word) = tokio::join!(
        index.search_fuzzy(&query, query.len() / 4),
        index.search_match(&query, MatchMode::Substring)
    );

    // 結果をマージ
    let mut unique_words: HashSet<String> = HashSet::new();
    for w in like_word.into_iter().chain(match_word.into_iter()) {
        unique_words.insert(w);
    }

    unique_words.into_iter().collect()
}