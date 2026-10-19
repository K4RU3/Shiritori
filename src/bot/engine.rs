//! ゲームコアエンジン
//!
//! すべての状態遷移はルームの write lock を取得してから完結する
//! 「操作完結型」設計。メッセージ送信・編集もロック内で実施することで、
//! v3の並行編集(他回答による vote_state の競合上書き)問題を解消している。
//!
//! 規模は単一チャンネル程度を想定しており、ロック保持中のネットワークIOは許容する。

use crate::{
    data::{RoomManager, RoomState, VoteData},
    words::{SearchMode, WordStore},
};

use super::{Bot, Messenger, SharedMessenger, message as m};

/// 類似既出単語を検索する(あいまい + 部分一致をマージ)
pub fn like_words(words: &WordStore, word: &str) -> Vec<String> {
    let count = WordStore::normalize(word).chars().count();
    let mut results = words.search_with_dist(word, SearchMode::Fuzzy, count / 4);
    results.extend(words.search(word, SearchMode::Substring));

    // 重複排除 + ソート
    results.sort();
    results.dedup();
    // 自身と完全一致する語は「類似」から除外
    let normalized = WordStore::normalize(word);
    results.retain(|w| *w != normalized);
    results
}

/// 進行中の投票メッセージを現在の状態で再構築して編集する
///
/// ルームのロックを保持したまま呼び出すこと。
async fn render_vote_edit(
    msgr: &dyn Messenger,
    room_id: u64,
    room: &RoomState,
) -> Result<(), String> {
    let Some(vote) = room.vote.as_ref() else {
        return Ok(());
    };

    let like = like_words(&room.words, &vote.word);
    let body = m::build_vote_message(vote, &room.user_queue, &like, None);
    msgr.edit(room_id, vote.message_id, body)
        .await
        .map_err(|e| format!("投票メッセージ編集に失敗: {e}"))
}

/// チャンネルをしりとりルームとして登録する
///
/// 戻り値: 新規登録された場合 true
pub async fn register_room(bot: &Bot, room_id: u64) -> bool {
    if bot.manager.has_room(room_id).await {
        false
    } else {
        let room = bot.manager.get_or_new_room(room_id).await;
        let _ = room.read().await;
        true
    }
}

/// 英語単語の回答を受け付ける
///
/// - 順番でなければ受理しない
/// - 辞書にある単語と完全一致なら即時棄却
/// - 進行中の投票がある場合、旧投票メッセージを「無効化」に確定させ、
///   回答者の最新回答で投票を出し直す
pub async fn try_word(bot: &Bot, msgr: &SharedMessenger, room_id: u64, user_id: u64, word: &str) {
    if !bot.manager.has_room(room_id).await {
        return; // 未登録チャンネルでは何もしない
    }

    let room_arc = bot.manager.get_or_new_room(room_id).await;
    let word = WordStore::normalize(word);
    if word.is_empty() {
        return;
    }

    // プレースホルダー送信(メッセージID取得のため)
    let placeholder_id = match msgr.send(room_id, m::placeholder()).await {
        Ok(id) => id,
        Err(e) => {
            eprintln!("メッセージ送信に失敗: {e}");
            return;
        }
    };

    {
        let mut room = room_arc.write().await;

        // キューが未定義 → 表示のみ
        if room.user_queue.is_empty() {
            let _ = msgr.edit(room_id, placeholder_id, m::queue_unset()).await;
            return;
        }

        // 順番チェック
        let front = room.user_queue[0];
        if front != user_id {
            let _ = msgr
                .edit(room_id, placeholder_id, m::not_your_turn(front))
                .await;
            return;
        }

        // 完全一致(既出) → 即時棄却
        if room.words.contains(&word) {
            let _ = msgr
                .edit(room_id, placeholder_id, m::already_answered(&word))
                .await;
            return;
        }

        // 既存投票があれば無効化して確定
        if let Some(old) = room.vote.take() {
            let old_id = old.message_id;
            let old_word = old.word.clone();
            let _ = msgr
                .edit(room_id, old_id, m::vote_invalidated(&old_word))
                .await;
        }

        // 類似既出単語(ロック内で計算)
        let like = like_words(&room.words, &word);

        // 投票開始
        room.vote = Some(VoteData {
            message_id: placeholder_id,
            word: word.clone(),
            user_id,
            good: vec![],
            bad: vec![],
        });

        // 初期メッセージ構築(意味は検索中)
        let body =
            m::build_vote_message(room.vote.as_ref().unwrap(), &room.user_queue, &like, None);
        if let Err(e) = msgr.edit(room_id, placeholder_id, body).await {
            eprintln!("投票メッセージ編集に失敗: {e}");
        }
    }

    // 意味検索をバックグラウンドで実行し、完了後に投票メッセージを更新
    // (投票が無効化/決着済みの場合は何もしない)
    let bot = bot.clone();
    let msgr = msgr.clone();
    let word_bg = word.clone();
    tokio::spawn(async move {
        let meaning = bot.meaning.get_meaning(word_bg.clone()).await;
        let room_arc = bot.manager.get_or_new_room(room_id).await;

        let latest = {
            let room = room_arc.read().await;
            room.vote
                .as_ref()
                .is_some_and(|v| v.message_id == placeholder_id && v.word == word_bg)
        };
        if !latest {
            return;
        }

        let (vote_snapshot, body) = {
            let room = room_arc.read().await;
            let Some(vote) = room.vote.as_ref() else {
                return;
            };
            if vote.message_id != placeholder_id || vote.word != word_bg {
                return;
            }
            let like = like_words(&room.words, &word_bg);
            let body = m::build_vote_message(vote, &room.user_queue, &like, Some(&meaning));
            (vote.clone(), body)
        };

        if let Err(e) = msgr.edit(room_id, vote_snapshot.message_id, body).await {
            eprintln!("意味更新で投票メッセージ編集に失敗: {e}");
        }
    });
}

/// 投票処理(リアクション / slashコマンド 共通)
///
/// - `good`: 有効投票かどうか(falseなら否決票)
/// - `cancel`: 票の取り消し
/// - 過半数に到達した時点で決着する(可決=辞書登録+キュー回転/否決=再回答)
pub async fn apply_vote(
    bot: &Bot,
    msgr: &SharedMessenger,
    room_id: u64,
    user_id: u64,
    good: bool,
    cancel: bool,
) {
    if !bot.manager.has_room(room_id).await {
        return;
    }

    let room_arc = bot.manager.get_or_new_room(room_id).await;

    {
        let mut room = room_arc.write().await;

        // 進行中投票の確認としきい値計算(可変借用前に完了させる)
        let Some(existing) = room.vote.as_ref() else {
            return;
        };
        if existing.user_id == user_id {
            return; // 回答者自身の投票は無効
        }

        let total_voters = room
            .user_queue
            .iter()
            .filter(|&&u| u != existing.user_id)
            .count();
        let threshold = m::majority_threshold(total_voters);

        // 票をローカルに取り出して更新
        let mut vote = room.vote.take().unwrap();
        if cancel {
            vote.good.retain(|&u| u != user_id);
            vote.bad.retain(|&u| u != user_id);
        } else if good {
            if !vote.good.contains(&user_id) {
                vote.good.push(user_id);
            }
            vote.bad.retain(|&u| u != user_id);
        } else {
            if !vote.bad.contains(&user_id) {
                vote.bad.push(user_id);
            }
            vote.good.retain(|&u| u != user_id);
        }

        let good_count = vote.good.len();
        let bad_count = vote.bad.len();

        if good_count >= threshold {
            // 可決: 辞書登録 + キュー回転
            let msg_id = vote.message_id;
            let final_msg = {
                room.user_queue.rotate_left(1);
                let next = room.user_queue.first().copied();
                m::vote_accepted(&vote, good_count, bad_count, next)
            };

            room.words.add_word(&vote.word);
            room.vote = None;

            let _ = msgr.edit(room_id, msg_id, final_msg).await;
        } else if bad_count >= threshold {
            // 否決: 再回答待ち
            let msg_id = vote.message_id;
            let final_msg = m::vote_rejected(&vote, good_count, bad_count);

            room.vote = None;

            let _ = msgr.edit(room_id, msg_id, final_msg).await;
        } else {
            // 進行中: 更新後の票を書き戻して表示を更新
            room.vote = Some(vote);
            if let Err(e) = render_vote_edit(&**msgr, room_id, &room).await {
                eprintln!("{e}");
            }
        }
    }
}

/// リアクションイベントの処理
///
/// - 👍/👎のみ反応
/// - 対象は進行中の投票メッセージのみ
/// - 排他: 片方を付けたらもう片方をDiscordから削除
pub async fn reaction_changed(
    bot: &Bot,
    msgr: &SharedMessenger,
    room_id: u64,
    message_id: u64,
    user_id: u64,
    emoji: &str,
    added: bool,
) {
    if emoji != "👍" && emoji != "👎" {
        return;
    }
    if !bot.manager.has_room(room_id).await {
        return;
    }

    // 進行中の投票メッセージのみ処理する
    let is_target = {
        let room_arc = bot.manager.get_or_new_room(room_id).await;
        let room = room_arc.read().await;
        room.vote
            .as_ref()
            .is_some_and(|v| v.message_id == message_id)
    };
    if !is_target {
        return;
    }

    // 排他: 👍/👎 は同時に持てない
    if added {
        let opposite = if emoji == "👍" { "👎" } else { "👍" };
        if let Err(e) = msgr
            .delete_reaction(room_id, message_id, user_id, opposite)
            .await
        {
            eprintln!("排他リアクション削除に失敗: {e}");
        }
    }

    apply_vote(bot, msgr, room_id, user_id, emoji == "👍", !added).await;
}

/// 順番設定コマンド
///
/// users が空の場合は現状表示のみ。
pub async fn set_queue(
    msgr: &SharedMessenger,
    manager: &RoomManager,
    room_id: u64,
    users: Vec<u64>,
) {
    let room_arc = manager.get_or_new_room(room_id).await;
    let message = {
        let mut room = room_arc.write().await;
        let prev = room.user_queue.clone();
        let message = m::queue_set(&users, &prev);
        if !users.is_empty() {
            room.user_queue = users;
        }
        message
    };

    let _ = msgr.send(room_id, message).await;
}

/// キューへユーザーを追加
pub async fn add_queue(msgr: &SharedMessenger, manager: &RoomManager, room_id: u64, user_id: u64) {
    let room_arc = manager.get_or_new_room(room_id).await;
    let message = {
        let mut room = room_arc.write().await;
        let prev = room.user_queue.clone();
        room.user_queue.push(user_id);
        m::queue_added(&prev, user_id)
    };

    let _ = msgr.send(room_id, message).await;
}

/// 辞書へ単語を追加
pub async fn add_words(
    msgr: &SharedMessenger,
    manager: &RoomManager,
    room_id: u64,
    words: Vec<String>,
) {
    let room_arc = manager.get_or_new_room(room_id).await;
    let added = {
        let mut room = room_arc.write().await;
        room.words.add_words(words.iter().map(|w| w.as_str()))
    };
    let _ = msgr.send(room_id, m::words_added(&added)).await;
}

/// 単語検索
///
/// ephemeral=true ならslashコマンド応答として、falseならチャンネル投稿する。
pub async fn find_words(
    msgr: &SharedMessenger,
    manager: &RoomManager,
    room_id: u64,
    query: &str,
    mode: SearchMode,
    max_dist: Option<usize>,
    ephemeral: bool,
) {
    if !manager.has_room(room_id).await {
        return;
    }
    let room_arc = manager.get_or_new_room(room_id).await;
    let message = {
        let room = room_arc.read().await;
        let dist = max_dist.unwrap_or_else(|| WordStore::normalize(query).chars().count() / 4);
        let results = room.words.search_with_dist(query, mode, dist);
        m::find_result(query, &results)
    };

    if ephemeral {
        let _ = msgr.reply_ephemeral(message).await;
    } else {
        let _ = msgr.send(room_id, message).await;
    }
}

/// 簡易検索(?コマンド): あいまい + 部分一致をマージする
pub async fn find_words_combined(
    msgr: &SharedMessenger,
    manager: &RoomManager,
    room_id: u64,
    query: &str,
    ephemeral: bool,
) {
    if !manager.has_room(room_id).await {
        return;
    }
    let room_arc = manager.get_or_new_room(room_id).await;
    let message = {
        let room = room_arc.read().await;
        let dist = WordStore::normalize(query).chars().count() / 4;
        let mut results = room.words.search_with_dist(query, SearchMode::Fuzzy, dist);
        results.extend(room.words.search(query, SearchMode::Substring));
        results.sort();
        results.dedup();
        m::find_result(query, &results)
    };

    if ephemeral {
        let _ = msgr.reply_ephemeral(message).await;
    } else {
        let _ = msgr.send(room_id, message).await;
    }
}

/// 順番スキップ
pub async fn skip(msgr: &SharedMessenger, manager: &RoomManager, room_id: u64, len: usize) {
    let room_arc = manager.get_or_new_room(room_id).await;
    let (skipped, queue, dist) = {
        let mut room = room_arc.write().await;
        if room.user_queue.is_empty() {
            (None, Vec::<u64>::new(), 0)
        } else {
            let size = room.user_queue.len();
            let skipped = room.user_queue[0];
            room.user_queue.rotate_left(len % size);
            room.vote = None;
            (Some(skipped), room.user_queue.clone(), len.min(size))
        }
    };

    let _ = msgr
        .send(room_id, m::skip_done(skipped, &queue, dist))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::MessengerResult;
    use crate::{RoomManager, data::VoteData, mean::NoopMeaningProvider};
    use serenity::async_trait;
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
        time::Duration,
    };

    /// テスト用フェイクメッセンジャー
    #[derive(Default)]
    pub struct FakeMessenger {
        events: Mutex<Vec<Event>>,
        next_id: AtomicU64,
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Event {
        Send {
            room_id: u64,
            msg_id: u64,
            content: String,
        },
        Edit {
            room_id: u64,
            msg_id: u64,
            content: String,
        },
        DeleteReaction {
            room_id: u64,
            msg_id: u64,
            user_id: u64,
            emoji: String,
        },
        ReplyEphemeral {
            content: String,
        },
    }

    #[async_trait]
    impl Messenger for FakeMessenger {
        async fn send(&self, room_id: u64, content: String) -> MessengerResult<u64> {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
            self.events.lock().unwrap().push(Event::Send {
                room_id,
                msg_id: id,
                content,
            });
            Ok(id)
        }

        async fn edit(&self, room_id: u64, msg_id: u64, content: String) -> MessengerResult<()> {
            self.events.lock().unwrap().push(Event::Edit {
                room_id,
                msg_id,
                content,
            });
            Ok(())
        }

        async fn delete_reaction(
            &self,
            room_id: u64,
            msg_id: u64,
            user_id: u64,
            emoji: &str,
        ) -> MessengerResult<()> {
            self.events.lock().unwrap().push(Event::DeleteReaction {
                room_id,
                msg_id,
                user_id,
                emoji: emoji.to_string(),
            });
            Ok(())
        }

        async fn reply_ephemeral(&self, content: String) -> MessengerResult<()> {
            self.events
                .lock()
                .unwrap()
                .push(Event::ReplyEphemeral { content });
            Ok(())
        }
    }

    impl FakeMessenger {
        /// 指定メッセージIDに対する最後の編集内容
        fn last_edit(&self, msg_id: u64) -> Option<String> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    Event::Edit {
                        msg_id: id,
                        content,
                        ..
                    } if *id == msg_id => Some(content.clone()),
                    _ => None,
                })
                .next_back()
        }

        /// 指定メッセージIDを対象にしたリアクション削除イベント
        fn deletions(&self, msg_id: u64) -> Vec<(u64, String)> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    Event::DeleteReaction {
                        msg_id: id,
                        user_id,
                        emoji,
                        ..
                    } if *id == msg_id => Some((*user_id, emoji.clone())),
                    _ => None,
                })
                .collect()
        }

        /// 全送信済みメッセージの内容
        fn sends(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    Event::Send { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .collect()
        }

        /// 送信(Send)イベント数
        fn send_count(&self) -> usize {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| matches!(e, Event::Send { .. }))
                .count()
        }

        /// 全ephemeral返信の内容
        fn replies(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    Event::ReplyEphemeral { content } => Some(content.clone()),
                    _ => None,
                })
                .collect()
        }
    }

    /// テスト用Bot(未登録状態)
    fn test_bot() -> Bot {
        Bot::new(
            RoomManager::new(),
            std::sync::Arc::new(NoopMeaningProvider),
            None,
        )
    }

    /// バックグラウンドの意味検索タスクが完了するまで待つ(決定常にクリア)
    pub(crate) async fn settle_background_tasks() {
        for _ in 0..5 {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// 検証ヘルパー(asyncコンテキスト内で使う)
    pub(crate) async fn read_room(
        bot: &Bot,
        room_id: u64,
    ) -> (Vec<u64>, Option<VoteData>, Vec<String>) {
        let room_arc = bot.manager.get_or_new_room(room_id).await;
        let room = room_arc.read().await;
        (
            room.user_queue.clone(),
            room.vote.clone(),
            room.words.words().to_vec(),
        )
    }

    fn word_of(vote: &Option<VoteData>) -> Option<String> {
        vote.as_ref().map(|v| v.word.clone())
    }

    #[tokio::test]
    async fn unregistered_channel_does_nothing() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        try_word(&bot, &msgr, 1000, 1, "apple").await;
        apply_vote(&bot, &msgr, 1000, 2, true, false).await;
        // 何も送信されず、部屋も作られない
        assert!(fake.sends().is_empty());
        assert!(!bot.manager.has_room(1000).await);
    }

    #[tokio::test]
    async fn queue_empty_shows_unset_message() {
        let bot = test_bot();
        // 部屋だけ作る(キューは空)
        let _ = bot.manager.get_or_new_room(1000).await;

        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        try_word(&bot, &msgr, 1000, 1, "apple").await;

        // プレースホルダーが編集され「順番が定義されていません」になる
        let placeholder = 1;
        let edited = fake.last_edit(placeholder).unwrap();
        assert_eq!(edited, m::queue_unset());

        let (queue, vote, _) = read_room(&bot, 1000).await;
        assert!(queue.is_empty());
        assert!(vote.is_none());
    }

    #[tokio::test]
    async fn out_of_turn_rejected() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;

        try_word(&bot, &msgr, 1000, 2, "apple").await;

        let placeholder = 2; // 1つ目が順番設定メッセージ
        let edited = fake.last_edit(placeholder).unwrap();
        assert!(edited.contains("現在は <@1> の番です"));

        let (queue, vote, words) = read_room(&bot, 1000).await;
        assert_eq!(queue, vec![1, 2, 3]);
        assert!(vote.is_none());
        assert!(words.is_empty());
    }

    #[tokio::test]
    async fn vote_flow_starts_and_live_updates() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;

        try_word(&bot, &msgr, 1000, 1, "apple").await;
        settle_background_tasks().await;

        // 投票メッセージ(プレースホルダー=2つ目の送信)
        let vote_msg = 2;
        let body = fake.last_edit(vote_msg).unwrap();
        assert!(body.contains("<@1> が 「apple」 を回答しました"));
        assert!(body.contains("(0 / 2 投票済み"));
        assert!(body.contains("過半数 2票"));

        let (_, vote, _) = read_room(&bot, 1000).await;
        assert_eq!(word_of(&vote), Some("apple".to_string()));
        assert_eq!(vote.as_ref().unwrap().message_id, vote_msg);

        // 1票目: 表示だけ更新
        apply_vote(&bot, &msgr, 1000, 2, true, false).await;
        let body = fake.last_edit(vote_msg).unwrap();
        assert!(body.contains("(1 / 2 投票済み"));
        let (_, vote, words) = read_room(&bot, 1000).await;
        assert!(vote.is_some());
        assert!(words.is_empty());

        // 2票目: 過半数 → 可決
        apply_vote(&bot, &msgr, 1000, 3, true, false).await;
        let body = fake.last_edit(vote_msg).unwrap();
        assert!(body.contains("可決され"));
        assert!(body.contains("辞書に登録"));
        assert!(body.contains("次は <@2> の番"));

        let (queue, vote, words) = read_room(&bot, 1000).await;
        assert_eq!(queue, vec![2, 3, 1]);
        assert!(vote.is_none());
        assert!(words.contains(&"apple".to_string()));
    }

    #[tokio::test]
    async fn rejected_vote_keeps_turn_and_word() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;

        try_word(&bot, &msgr, 1000, 1, "apple").await;
        apply_vote(&bot, &msgr, 1000, 2, false, false).await;
        apply_vote(&bot, &msgr, 1000, 3, false, false).await;

        let vote_msg = 2;
        let body = fake.last_edit(vote_msg).unwrap();
        assert!(body.contains("否決され"));
        assert!(body.contains("<@1> は、別の回答をしてください"));

        let (queue, vote, words) = read_room(&bot, 1000).await;
        assert_eq!(queue, vec![1, 2, 3]); // 順番は維持
        assert!(vote.is_none());
        assert!(words.is_empty()); // 辞書には入らない
    }

    #[tokio::test]
    async fn cancel_vote_removes_my_ballot() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;

        try_word(&bot, &msgr, 1000, 1, "apple").await;
        apply_vote(&bot, &msgr, 1000, 2, true, false).await;
        apply_vote(&bot, &msgr, 1000, 2, true, true).await; // 取り消し

        let vote_msg = 2;
        let body = fake.last_edit(vote_msg).unwrap();
        assert!(body.contains("(0 / 2 投票済み"));

        let (queue, vote, words) = read_room(&bot, 1000).await;
        assert!(vote.is_some()); // 進行中のまま
        assert!(words.is_empty());
        assert_eq!(queue, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn exact_duplicate_rejected_without_vote() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;
        add_words(&msgr, &bot.manager, 1000, vec!["apple".to_string()]).await;

        let before = 2; // 順番設定 + 辞書追加で2件送信済み(プレースホルダーは before+1)
        try_word(&bot, &msgr, 1000, 1, "APPLE").await; // 大文字/空白も正規化で完全一致

        let edited = fake.last_edit(before + 1).unwrap();
        assert_eq!(edited, m::already_answered("apple"));

        let (_, vote, words) = read_room(&bot, 1000).await;
        assert!(vote.is_none());
        assert_eq!(words, vec!["apple".to_string()]);
    }

    #[tokio::test]
    async fn answer_switch_invalidates_previous_vote() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;

        try_word(&bot, &msgr, 1000, 1, "apple").await; // 投票A (msg 2)
        try_word(&bot, &msgr, 1000, 1, "banana").await; // 投票B (msg 3)
        settle_background_tasks().await;

        // 旧投票メッセージは無効化で確定
        let old = fake.last_edit(2).unwrap();
        assert_eq!(old, m::vote_invalidated("apple"));

        // 現在の投票は banana (最新)
        let (_, vote, _) = read_room(&bot, 1000).await;
        let v = vote.unwrap();
        assert_eq!(v.word, "banana");
        assert_eq!(v.message_id, 3);
        assert_eq!(v.good.len(), 0);
    }

    #[tokio::test]
    async fn reactions_drive_votes_with_exclusive_removal() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;

        try_word(&bot, &msgr, 1000, 1, "apple").await;
        let vote_msg = 2;

        // 👍 → 排他で 👎 が削除される
        reaction_changed(&bot, &msgr, 1000, vote_msg, 2, "👍", true).await;
        let deletions = fake.deletions(vote_msg);
        assert_eq!(deletions, vec![(2, "👎".to_string())]);

        // 別絵文字・旧メッセージには反応しない
        let events_before = fake.send_count();
        reaction_changed(&bot, &msgr, 1000, vote_msg, 2, "🔥", true).await;
        reaction_changed(&bot, &msgr, 1000, 999, 2, "👍", true).await; // 存在しない投票メッセージ
        assert_eq!(fake.sends().len(), events_before);
        let delete_count = {
            let mut count = 0;
            for e in fake.events.lock().unwrap().iter() {
                if matches!(e, Event::DeleteReaction { .. }) {
                    count += 1;
                }
            }
            count
        };
        assert_eq!(delete_count, 1);

        // 過半数で可決
        reaction_changed(&bot, &msgr, 1000, vote_msg, 3, "👍", true).await;
        let body = fake.last_edit(vote_msg).unwrap();
        assert!(body.contains("可決され"));
        let (_, vote, words) = read_room(&bot, 1000).await;
        assert!(vote.is_none());
        assert!(words.contains(&"apple".to_string()));
    }

    #[tokio::test]
    async fn answerer_cannot_vote_self() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;

        try_word(&bot, &msgr, 1000, 1, "apple").await;
        apply_vote(&bot, &msgr, 1000, 1, true, false).await; // 回答者自身

        let (_, vote, _) = read_room(&bot, 1000).await;
        assert_eq!(vote.as_ref().unwrap().good.len(), 0);
        // 回答者はキュー参加者から除外されているため総数も変わらない
    }

    #[tokio::test]
    async fn set_and_add_queue_display() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();

        // 空キューで表示のみ
        set_queue(&msgr, &bot.manager, 1000, vec![]).await;
        assert_eq!(fake.sends().last().unwrap().clone(), m::queue_unset());

        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;
        let sent = fake.sends().last().unwrap().clone();
        assert!(sent.contains("の順番に設定しました"));
        assert!(sent.contains("次は <@1> の番"));

        add_queue(&msgr, &bot.manager, 1000, 4).await;
        let sent = fake.sends().last().unwrap().clone();
        assert!(sent.contains("<@4> をゲームに追加"));
        assert!(sent.contains("次は <@1>"));

        let (queue, _, _) = read_room(&bot, 1000).await;
        assert_eq!(queue, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn skip_rotates_and_clears_vote() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2, 3]).await;

        try_word(&bot, &msgr, 1000, 1, "apple").await;
        skip(&msgr, &bot.manager, 1000, 2).await;

        let sent = fake.sends().last().unwrap().clone();
        assert!(sent.contains("2人 をスキップ"));
        assert!(sent.contains("次は <@3> の番"));

        let (queue, vote, words) = read_room(&bot, 1000).await;
        assert_eq!(queue, vec![3, 1, 2]);
        assert!(vote.is_none()); // 投票は解除
        assert!(words.is_empty());
    }

    #[tokio::test]
    async fn add_words_reports_added_only() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        let _ = bot.manager.get_or_new_room(1000).await;

        add_words(
            &msgr,
            &bot.manager,
            1000,
            vec![
                "apple".to_string(),
                "Apple".to_string(), // 重複
                "grape".to_string(),
            ],
        )
        .await;

        let sent = fake.sends().last().unwrap().clone();
        assert!(sent.contains("apple"));
        assert!(sent.contains("grape"));
        // "Apple"は正規化で重複のため1回のみ
        assert_eq!(sent.matches("apple").count(), 1);

        let (_, _, words) = read_room(&bot, 1000).await;
        assert_eq!(words, vec!["apple".to_string(), "grape".to_string()]);
    }

    #[tokio::test]
    async fn find_words_reports_matches() {
        let bot = test_bot();
        let fake = std::sync::Arc::new(FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        set_queue(&msgr, &bot.manager, 1000, vec![1, 2]).await;
        add_words(&msgr, &bot.manager, 1000, vec!["apple".to_string()]).await;

        find_words(
            &msgr,
            &bot.manager,
            1000,
            "apple",
            SearchMode::Exact,
            None,
            true,
        )
        .await;
        let reply = fake.replies().last().unwrap().clone();
        assert!(reply.contains("apple"));

        find_words(
            &msgr,
            &bot.manager,
            1000,
            "zzz",
            SearchMode::Fuzzy,
            Some(0),
            true,
        )
        .await;
        let reply = fake.replies().last().unwrap().clone();
        assert!(reply.contains("見つかりませんでした"));
    }

    #[tokio::test]
    async fn like_words_merges_fuzzy_and_substring() {
        let mut words = WordStore::new();
        words.add_words(["apple", "apply", "grape", "applic"]);
        let like = like_words(&words, "app");
        // 部分一致(apple, apply, applic)に加え、あいまい距離0(apple)をマージ
        assert!(like.contains(&"apple".to_string()));
        assert!(like.contains(&"apply".to_string()));
        assert!(like.contains(&"applic".to_string()));
        assert!(!like.contains(&"grape".to_string()));
    }
}

/// メンバーコマンド(!queue等)の実行権限判定
///
/// キュー参加者またはキュー未定義時は true。
pub async fn can_manage(manager: &RoomManager, room_id: u64, user_id: u64) -> bool {
    let room_arc = manager.get_or_new_room(room_id).await;
    let room = room_arc.read().await;
    room.user_queue.is_empty() || room.user_queue.contains(&user_id)
}

#[cfg(test)]
mod persistence_repro_tests {
    use crate::{RoomManager, bot::SharedMessenger, mean::NoopMeaningProvider};

    /// 実際の運用フロー(起動→プレイ→可決→保存→「再起動」→読み込み)を再現し、
    /// 辞書・キュー・表示名が再起動後も残ることを検証する
    #[tokio::test]
    async fn words_survive_save_and_reload() {
        use std::fs;

        let dir = "/tmp/shiritori_engine_e2e";
        let _ = fs::remove_dir_all(dir);
        fs::create_dir_all(dir).unwrap();
        let path = format!("{dir}/data.json");

        // ---- 1回目の起動: ルーム登録 → プレイ → 可決 → 保存 ----
        let manager = RoomManager::new();
        let bot = crate::bot::Bot::new(
            manager.clone(),
            std::sync::Arc::new(NoopMeaningProvider),
            None,
        );
        // 表示名(メンション/ready時のフロー相当)
        manager
            .set_room_name(1000, "Test Guild/general".to_string())
            .await;
        let fake = std::sync::Arc::new(super::tests::FakeMessenger::default());
        let msgr: SharedMessenger = fake.clone();
        super::set_queue(&msgr, &manager, 1000, vec![1, 2, 3]).await;
        super::try_word(&bot, &msgr, 1000, 1, "apple").await;
        super::tests::settle_background_tasks().await;
        let vote_msg = 2; // set_queueの送信(1)の次のプレースホルダー
        // 過半数で可決(回答者1を除く2人中2票)
        super::reaction_changed(&bot, &msgr, 1000, vote_msg, 2, "👍", true).await;
        super::reaction_changed(&bot, &msgr, 1000, vote_msg, 3, "👍", true).await;
        let (_, _, words) = super::tests::read_room(&bot, 1000).await;
        assert!(words.contains(&"apple".to_string()));

        manager.save_file(&path).await.unwrap();
        let disk = fs::read_to_string(&path).unwrap();
        assert!(disk.contains("apple"), "保存時に単語が含まれている: {disk}");

        // ---- 再起動: ファイルから読み込み直す ----
        let reloaded = RoomManager::load_file(&path).await.unwrap();
        assert!(reloaded.has_room(1000).await);
        let room_arc = reloaded.get_or_new_room(1000).await;
        let room = room_arc.read().await;
        assert!(room.words.contains("apple"), "再起動後も単語が残る");
        assert_eq!(room.user_queue, vec![2, 3, 1]);
        assert_eq!(
            room.name.as_deref(),
            Some("Test Guild/general"),
            "表示名も残る"
        );
        assert!(room.vote.is_none());

        // ---- 再起動後も完全一致は棄却される ----
        drop(room);
        let bot2 = crate::bot::Bot::new(
            reloaded.clone(),
            std::sync::Arc::new(NoopMeaningProvider),
            None,
        );
        let fake2 = std::sync::Arc::new(super::tests::FakeMessenger::default());
        let msgr2: SharedMessenger = fake2.clone();
        super::try_word(&bot2, &msgr2, 1000, 2, "APPLE").await;
        let (_, vote, _) = super::tests::read_room(&bot2, 1000).await;
        assert!(vote.is_none(), "既出は即棄却されるため投票は開始されない");

        let _ = fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod permission_tests {
    use crate::{RoomManager, bot::engine::can_manage};

    #[tokio::test]
    async fn can_manage_for_queue_members_and_initial_setup() {
        let manager = RoomManager::new();
        // キュー未定義 → 誰でも実行可
        assert!(can_manage(&manager, 1000, 777).await);

        let msgr: crate::bot::SharedMessenger =
            std::sync::Arc::new(super::tests::FakeMessenger::default());
        super::set_queue(&msgr, &manager, 1000, vec![1, 2, 3]).await;

        assert!(can_manage(&manager, 1000, 1).await);
        assert!(can_manage(&manager, 1000, 3).await);
        assert!(!can_manage(&manager, 1000, 777).await);
    }
}
