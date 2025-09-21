use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TryMessageBuilder {
    pub user: Option<u64>,
    pub word: Option<String>,
    pub mean: Option<String>,
    pub all_users: Vec<u64>,
    pub vote_good: Vec<u64>,
    pub vote_bad: Vec<u64>,
    pub like_words: Option<Vec<String>>,
}

impl TryMessageBuilder {
    pub fn new() -> Self { Self::default() }

    pub fn init(user_id: u64, word: String, users: Vec<u64>) -> Self {
        Self {
            user: Some(user_id),
            word: Some(word),
            all_users: users,
            ..Default::default()
        }
    }

    pub fn build(&self) -> String {
        let mut msg = String::new();

        // まず word が存在するかチェック
        if let Some(word) = &self.word {
            // すでに回答済みなら固定メッセージを返す
            if self.like_words.as_ref().map_or(false, |v| v.contains(word)) {
                if let Some(user) = self.user {
                    return format!(
                        "{} はすでに回答されています。\n<@{}> は、別の回答を提出してください。",
                        word, user
                    );
                } else {
                    return format!("{} はすでに回答されています。", word);
                }
            }
        }

        // 1. 回答した単語
        if let (Some(user), Some(word)) = (self.user, &self.word) {
            msg.push_str(&format!("<@{}> が {} を回答しました。\n", user, word));
        }

        // 2. 単語の意味
        if let Some(word) = &self.word {
            if let Some(mean) = &self.mean {
                msg.push_str(&format!("{} の意味は以下の通りです\n```\n{}\n```\n", word, mean));
            } else {
                msg.push_str(&format!("{} の意味を検索中...\n", word));
            }
        }

        // 3. 類似単語
        if let Some(words) = &self.like_words {
            if !words.is_empty() {
                msg.push_str("類似する単語は以下の通りです:\n```\n");
                for w in words {
                    msg.push_str(&format!("{}\n", w));
                }
                msg.push_str("```\n");
            } else if let Some(word) = &self.word {
                msg.push_str(&format!("{} に類似する単語は回答されていません。\n", word));
            }
        } else if let Some(word) = &self.word {
            msg.push_str(&format!("{} に類似する単語を検索中...\n", word));
        }

        // 4. 投票
        let total = self.all_users.len();
        let voted = self.vote_good.len() + self.vote_bad.len();

        let good_list = self.vote_good.iter().map(|v| format!("<@{}>", v)).collect::<Vec<_>>().join(", ");
        let bad_list  = self.vote_bad.iter().map(|v| format!("<@{}>", v)).collect::<Vec<_>>().join(", ");
        let not_voted_list = self.all_users
            .iter()
            .filter(|u| !self.vote_good.contains(u) && !self.vote_bad.contains(u))
            .map(|v| format!("<@{}>", v))
            .collect::<Vec<_>>()
            .join(", ");

        msg.push_str(&format!(":timer: 投票状況 ({} / {}人 投票済み)\n\n", voted, total));
        msg.push_str(&format!(":thumbsup: Good({}): {}\n", self.vote_good.len(), good_list));
        msg.push_str(&format!(":thumbsdown: Bad({}): {}\n", self.vote_bad.len(), bad_list));
        msg.push_str(&format!(":crab: 未投票({}): {}\n", total - voted, not_voted_list));

        msg
    }
}

pub fn generate_register_message(is_registered: bool) -> String {
    if is_registered {
        return "このチャンネルはすでに追加されています。".to_string();
    } else {
        return "このチャンネルをしりとりチャンネルとして追加しました。".to_string();
    }
}

pub fn generate_set_queue_message(users: &[u64], current_users: &[u64]) -> String {
    if users.is_empty() {
        if current_users.is_empty() {
            return "順番が設定されていません".to_string();
        } else {
            let current_list = current_users
                .iter()
                .map(|id| format!("<@{}>", id))
                .collect::<Vec<_>>()
                .join(" -> ");
            let next_user = format!("<@{}>", current_users[0]);
            return format!(
                "現在の順番: {}\n次は {} の番です。",
                current_list, next_user
            );
        }
    }

    let user_list = users.iter()
        .map(|id| format!("<@{}>", id))
        .collect::<Vec<_>>()
        .join(" -> ");

    let next_user = format!("<@{}>", users[0]);

    format!("{} の順番に設定しました。\n次は {} の番です。", user_list, next_user)
}

pub fn generate_add_queue_message(users: &[u64], add: u64) -> String {
    // 新しい queue を作る（追加ユーザー含む）
    let mut new_queue = users.to_vec();
    new_queue.push(add);

    // ユーザーリストを文字列化
    let user_list = new_queue.iter()
        .map(|id| format!("<@{}>", id))
        .collect::<Vec<_>>()
        .join(", ");

    // 次の人は queue の先頭
    let next_user = format!("<@{}>", new_queue[0]);

    format!(
        "<@{}> をゲームに追加しました。\n現在の順番は {} です。\n次は {} です。",
        add, user_list, next_user
    )
}

pub fn generate_find_message(word: &str, words: &[String]) -> String {
    if words.is_empty() {
        format!("「{}」に類似する単語は見つかりませんでした。", word)
    } else {
        let similar_words = words.join("\n"); // 改行で列挙
        format!(
            "「{}」に類似する単語を見つけました:\n{}",
            word, similar_words
        )
    }
}

pub fn generate_added_words_message(words: &[String]) -> String {
    if words.is_empty() {
        return "追加された単語はありません。".to_string();
    }

    let mut message = String::from("以下の単語を追加しました。```\n");

    for word in words {
        message.push_str(word);
        message.push('\n');
    }

    message.push_str("```");
    message
}
