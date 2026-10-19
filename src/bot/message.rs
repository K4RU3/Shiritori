//! Discord向けメッセージ生成(純関数)
//!
//! すべてのメッセージテキストはここで一貫生成する。
//! v3の `message.rs` 相当。テスト容易性のためDiscord依存を持たない。

use crate::data::VoteData;

/// ルール未定義(キュー空)時に表示するメッセージ
pub fn queue_unset() -> String {
    "ユーザーの順番が定義されていません。".to_string()
}

/// 回答済み単語(完全一致)の棄却メッセージ
pub fn already_answered(word: &str) -> String {
    format!("「{word}」はすでに回答されています。")
}

/// 順番でないユーザーへの棄却メッセージ
pub fn not_your_turn(front: u64) -> String {
    format!("現在は <@{front}> の番です。お待ちください。")
}

/// 投票メッセージ構築中表示
pub fn placeholder() -> String {
    "メッセージを構築中...".to_string()
}

/// 話者自身の回答を除く順番待ち人数(投票者数)
fn voter_count(user_queue: &[u64], answerer: u64) -> usize {
    user_queue.iter().filter(|&&u| u != answerer).count()
}

/// 進行中の投票メッセージ本体
///
/// - resembling: 類似既出単語一覧(重複排除・ソート済み想定)
/// - meaning: 意味テキスト(検索中なら None)
pub fn build_vote_message(
    vote: &VoteData,
    user_queue: &[u64],
    like_words: &[String],
    meaning: Option<&str>,
) -> String {
    let mut msg = format!(
        "<@{}> が 「{}」 を回答しました。\n\n",
        vote.user_id, vote.word
    );

    // 意味
    msg.push_str(&match meaning {
        Some(text) => format!("**意味**\n```\n{text}\n```\n\n"),
        None => "**意味**\n意味を検索中...\n\n".to_string(),
    });

    // 類似既出単語
    if like_words.is_empty() {
        msg.push_str(&format!(
            "**類似する既出単語**\n「{}」に類似する単語は見つかりませんでした。\n\n",
            vote.word
        ));
    } else {
        // Discordの2000文字上限を避けるため最大10件まで表示
        const MAX_LIKE_WORDS: usize = 10;
        msg.push_str("**類似する既出単語**\n```\n");
        for w in like_words.iter().take(MAX_LIKE_WORDS) {
            msg.push_str(w);
            msg.push('\n');
        }
        msg.push_str("```");
        if like_words.len() > MAX_LIKE_WORDS {
            msg.push_str(&format!("(他 {} 件)\n", like_words.len() - MAX_LIKE_WORDS));
        }
        msg.push('\n');
    }

    // 投票状況
    let total = voter_count(user_queue, vote.user_id);
    let voted = vote.good.len() + vote.bad.len();

    let good_list = mention_list(&vote.good);
    let bad_list = mention_list(&vote.bad);
    let not_voted_list = mention_list(
        &user_queue
            .iter()
            .filter(|&&u| u != vote.user_id && !vote.good.contains(&u) && !vote.bad.contains(&u))
            .copied()
            .collect::<Vec<u64>>(),
    );

    msg.push_str(&format!(
        ":timer: **投票状況** ({} / {} 投票済み / 過半数 {}票)\n",
        voted,
        total,
        majority_threshold(total)
    ));
    msg.push_str(&format!(
        ":thumbsup: 有効({}): {}\n",
        vote.good.len(),
        good_list
    ));
    msg.push_str(&format!(
        ":thumbsdown: 無効({}): {}\n",
        vote.bad.len(),
        bad_list
    ));
    msg.push_str(&format!(
        ":crab: 未投票({}): {}",
        total.saturating_sub(voted),
        if not_voted_list.is_empty() {
            "なし".to_string()
        } else {
            not_voted_list
        }
    ));

    msg
}

/// 過半数のしきい値 (= 投票者数 / 2 + 1)
pub fn majority_threshold(total_voters: usize) -> usize {
    total_voters / 2 + 1
}

fn mention_list(users: &[u64]) -> String {
    if users.is_empty() {
        "なし".to_string()
    } else {
        users
            .iter()
            .map(|id| format!("<@{id}>"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// 可決時の確定メッセージ(next: キュー回転後の次ユーザー)
pub fn vote_accepted(vote: &VoteData, good: usize, bad: usize, next_user: Option<u64>) -> String {
    let mut msg = format!(
        "**投票終了！** 結果: YES({good}) / NO({bad})\n「{}」は可決され、辞書に登録されました。",
        vote.word
    );
    if let Some(next) = next_user {
        msg.push_str(&format!("\n\n次は <@{next}> の番です。"));
    }
    msg
}

/// 否決時の確定メッセージ
pub fn vote_rejected(vote: &VoteData, good: usize, bad: usize) -> String {
    format!(
        "**投票終了！** 結果: NO({bad}) / YES({good})\n「{}」は否決されました。\n\n<@{}> は、別の回答をしてください。",
        vote.word, vote.user_id
    )
}

/// 進行中投票が別の回答に切り替えられたときの旧投票メッセージ
pub fn vote_invalidated(vote_word: &str) -> String {
    format!("「{vote_word}」の投票は無効化されました。")
}

/// チャンネル登録メッセージ
///
/// - fresh: 新規登録なら true
/// - dict_url: Web辞書の公開URL(設定時のみ案内を追記)
pub fn register_message(fresh: bool, dict_url: Option<&str>) -> String {
    let base = if fresh {
        "このチャンネルをしりとりチャンネルとして追加しました。".to_string()
    } else {
        "このチャンネルはすでにしりとりチャンネルとして登録されています。".to_string()
    };

    if let Some(url) = dict_url {
        format!(
            "{base}\nWeb辞書: {url}\n🎮 コマンド一覧:\n- `!queue @user...` 順番を設定\n- `!addqueue @user` 参加\n- `!addwords word, ...` 単語追加\n- `!skip [n]` スキップ\n- `?単語` 検索"
        )
    } else {
        base
    }
}

/// !queue(順番設定)メッセージ
///
/// users が空の場合は現在のキューを表示のみ行う
pub fn queue_set(users: &[u64], current_users: &[u64]) -> String {
    if users.is_empty() {
        if current_users.is_empty() {
            return queue_unset();
        }
        let current_list = current_users
            .iter()
            .map(|id| format!("<@{id}>"))
            .collect::<Vec<_>>()
            .join(" -> ");
        let next_user = format!("<@{}>", current_users[0]);
        return format!("現在の順番: {current_list}\n次は {next_user} の番です。");
    }

    let user_list = users
        .iter()
        .map(|id| format!("<@{id}>"))
        .collect::<Vec<_>>()
        .join(" -> ");
    let next_user = format!("<@{}>", users[0]);
    format!("{user_list} の順番に設定しました。\n次は {next_user} の番です。")
}

/// !addqueue(参加)メッセージ
pub fn queue_added(prev_queue: &[u64], user: u64) -> String {
    let mut new_queue = prev_queue.to_vec();
    new_queue.push(user);

    let user_list = new_queue
        .iter()
        .map(|id| format!("<@{id}>"))
        .collect::<Vec<_>>()
        .join(" -> ");
    let next_user = format!("<@{}>", new_queue[0]);

    format!(
        "<@{user}> をゲームに追加しました。\n現在の順番は {user_list} です。\n次は {next_user} です。"
    )
}

/// 検索結果メッセージ
pub fn find_result(query: &str, words: &[String]) -> String {
    if words.is_empty() {
        format!("「{query}」に一致する単語は見つかりませんでした。")
    } else {
        const MAX_FIND_WORDS: usize = 30;
        let mut list = String::new();
        for w in words.iter().take(MAX_FIND_WORDS) {
            list.push_str(w);
            list.push('\n');
        }
        if words.len() > MAX_FIND_WORDS {
            list.push_str(&format!("(他 {} 件)\n", words.len() - MAX_FIND_WORDS));
        }

        format!(
            "「{query}」に一致する単語を {} 件見つけました:\n```\n{list}```",
            words.len()
        )
    }
}

/// !addwords(辞書一括追加)メッセージ
pub fn words_added(added: &[String]) -> String {
    if added.is_empty() {
        return "追加された単語はありません。".to_string();
    }

    let mut message = String::from("以下の単語を辞書に追加しました。```\n");
    for word in added {
        message.push_str(word);
        message.push('\n');
    }
    message.push_str("```");
    message
}

/// !skipメッセージ
pub fn skip_done(skipped: Option<u64>, queue: &[u64], dist: usize) -> String {
    if queue.is_empty() || skipped.is_none() {
        return "スキップできるユーザーがいません。".to_string();
    }

    format!(
        "<@{}> 以降 {}人 をスキップしました！ 次は <@{}> の番です。",
        skipped.unwrap(),
        dist,
        queue.first().copied().unwrap_or(0)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_vote(user: u64, word: &str, good: Vec<u64>, bad: Vec<u64>) -> VoteData {
        VoteData {
            message_id: 1000,
            word: word.to_string(),
            user_id: user,
            good,
            bad,
        }
    }

    #[test]
    fn vote_message_contains_all_sections() {
        let vote = sample_vote(101, "apple", vec![102], vec![]);
        let msg = build_vote_message(
            &vote,
            &[101, 102, 103, 104],
            &["ample".to_string()],
            Some("リンゴ"),
        );

        assert!(msg.contains("<@101> が 「apple」 を回答しました"));
        assert!(msg.contains("リンゴ"));
        assert!(msg.contains("ample"));
        // 投票者数 = 3(回答者除く)、既投票1
        assert!(msg.contains("(1 / 3 投票済み"));
        assert!(msg.contains("有効(1)"));
        assert!(msg.contains("<@102>"));
        assert!(msg.contains("未投票(2)"));
        assert!(msg.contains("過半数 2票"));
    }

    #[test]
    fn vote_message_searching_meaning() {
        let vote = sample_vote(1, "cat", vec![], vec![]);
        let msg = build_vote_message(&vote, &[1, 2, 3], &[], None);
        assert!(msg.contains("意味を検索中..."));
        assert!(msg.contains("類似する単語は見つかりませんでした"));
    }

    #[test]
    fn vote_message_ignores_answering_user_in_voters() {
        // 回答者が自分自身に投票した形で渡されても、投票者数は回答者を除く
        let vote = sample_vote(1, "dog", vec![1], vec![]);
        let msg = build_vote_message(&vote, &[1, 2], &[], None);
        // 有効1つだが投票者数は1
        assert!(msg.contains("(1 / 1 投票済み"));
    }

    #[test]
    fn like_words_capped() {
        let like: Vec<String> = (0..25).map(|i| format!("w{i}")).collect();
        let vote = sample_vote(1, "x", vec![], vec![]);
        let msg = build_vote_message(&vote, &[1, 2], &like, None);
        assert!(msg.contains("(他 15 件)"));
        assert!(!msg.contains("w24")); // 上限超過は表示されない
    }

    #[test]
    fn vote_accepted_message() {
        let vote = sample_vote(1, "apple", vec![2, 3], vec![]);
        let msg = vote_accepted(&vote, 2, 0, Some(99));
        assert!(msg.contains("可決され"));
        assert!(msg.contains("辞書に登録"));
        assert!(msg.contains("次は <@99> の番"));
    }

    #[test]
    fn vote_rejected_message() {
        let vote = sample_vote(1, "apple", vec![], vec![2, 3]);
        let msg = vote_rejected(&vote, 0, 2);
        assert!(msg.contains("否決され"));
        assert!(msg.contains("<@1> は、別の回答をしてください。"));
    }

    #[test]
    fn invalidated_message_mentions_word() {
        let msg = vote_invalidated("apple");
        assert!(msg.contains("apple"));
        assert!(msg.contains("無効化されました"));
    }

    #[test]
    fn queue_messages() {
        assert_eq!(queue_unset(), "ユーザーの順番が定義されていません。");
        let set = queue_set(&[1, 2], &[7]);
        assert!(set.contains("<@1>"));
        assert!(set.contains("次は <@1> の番"));
        let empty_display = queue_set(&[], &[1, 2]);
        assert!(empty_display.contains("現在の順番: <@1> -> <@2>"));
        let added = queue_added(&[1], 2);
        assert!(added.contains("<@2> をゲームに追加"));
        assert!(added.contains("次は <@1>"));
    }

    #[test]
    fn register_message_with_dict_url() {
        let msg = register_message(true, Some("https://example.com"));
        assert!(msg.contains("追加しました"));
        assert!(msg.contains("https://example.com"));
        assert!(msg.contains("!queue @user..."));
        let fresh2 = register_message(false, None);
        assert!(fresh2.contains("すでに"));
        assert!(!fresh2.contains("Web辞書"));
    }

    #[test]
    fn skip_messages() {
        assert_eq!(
            skip_done(None, &[], 1),
            "スキップできるユーザーがいません。"
        );
        let msg = skip_done(Some(1), &[2, 3], 1);
        assert!(msg.contains("スキップしました"));
        assert!(msg.contains("次は <@2>"));
    }

    #[test]
    fn find_and_addwords_messages() {
        assert!(find_result("abc", &[]).contains("見つかりませんでした"));
        let found = find_result("abc", &["abc".to_string()]);
        assert!(found.contains("abc"));
        assert!(words_added(&[]).contains("追加された単語はありません"));
        let added = words_added(&["x1".to_string()]);
        assert!(added.contains("x1"));
    }
}
