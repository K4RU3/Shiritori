use reqwest::{Client};
use serde_json::json;

pub async fn get_word_mean_jp(word: String) -> String {
    let client = Client::new();
    let api_key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY not set");

    let prompt = build_prompt(word.as_str());

    let resp = match client
        .post("https://openrouter.ai/api/v1/chat/completions")
        .bearer_auth(api_key)
        .json(&json!({
            "model": "openai/gpt-oss-20b:free",
            "messages": [
                {"role": "user", "content": prompt}
            ]
        }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return format!("API request failed: {:?}", e),
    };

    let json_resp: serde_json::Value = match resp.json().await {
        Ok(j) => j,
        Err(e) => return format!("Failed to parse JSON: {:?}", e),
    };

    // レスポンスから生成されたテキストを取り出す
    let content = json_resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("No definition found")
        .to_string();

    content
}

fn build_prompt(word: &str) -> String {
    let word_like = &format!("単語: {}", word);
    let lines = vec![
        "以下の形式で、英単語の意味を日本語で教えてください。",
        "",
        "1. まず「意味」を簡潔にまとめる",
        "2. 品詞ごとに分けて書くが、存在しない品詞は絶対に書かない",
        "3. 名詞の場合は複数形も示し、例文を1つ添える",
        "4. 動詞・形容詞・副詞の場合も存在する場合のみ書く",
        "5. もし単語の先頭が大文字の場合は、固有名詞として扱い、品詞に関わらずその意味と例文を必ず書き、正式名称を書く",
        "6. Markdown形式で見やすく書く",
        "",
        "形式は次の通りです：",
        "",
        "**意味**",
        "(例：外見・量など) 同様な、類似の、似ていて",
        "",
        "**名詞**",
        "[複数形で] 単語",
        "例文：*This is an example.*",
        "→ これは例です。",
        "",
        "**動詞**",
        "単語（意味）",
        "例文：*Use it in a sentence.*",
        "→ 文で使ってみてください。",
        "",
        word_like
    ];

    lines.join("\n")
}
