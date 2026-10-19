//! 文字数基準のレーベンシュタイン距離
//!
//! v3実装はコスト表をバイト長(`b.len()`)で確保しつつ `chars()` でループしており、
//! マルチバイト文字を含む文字列で配列範囲外参照の可能性があった。
//! 本実装は常に `chars().count()` (文字数) を基準とする。

/// 2つの文字列間のレーベンシュタイン距離を返す(文字数基準)。
///
/// - 空文字列との距離は相手の文字数と一致する
/// - マルチバイト文字(日本語・絵文字など)も1文字として計算する
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_len = a.chars().count();
    let b_len = b.chars().count();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    // 1行分のコスト表のみ保持する標準的なDP
    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut curr: Vec<usize> = vec![0; b_len + 1];

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1) // 削除
                .min(curr[j] + 1) // 挿入
                .min(prev[j] + cost); // 置換
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_len]
}

#[cfg(test)]
mod tests {
    use super::levenshtein;

    #[test]
    fn identical_strings() {
        assert_eq!(levenshtein("hello", "hello"), 0);
        assert_eq!(levenshtein("こんにちは", "こんにちは"), 0);
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn empty_vs_word() {
        // 空文字列との距離は相手の文字数(バイト長ではない)
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "かな"), 2);
        assert_eq!(levenshtein("👍🔜", ""), 2);
    }

    #[test]
    fn known_distances_ascii() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
        assert_eq!(levenshtein("gumbo", "gambol"), 2);
    }

    #[test]
    fn multibyte_counts_as_one_char() {
        // v3実装はバイト長基準だったため、ここでマルチバイト対応を確認する
        // 「あいう」(3文字) vs 「かいう」(3文字) → 1
        assert_eq!(levenshtein("あいう", "かいう"), 1);
        // 絵文字1つを1文字として数える
        assert_eq!(levenshtein("🙂a", "🙃a"), 1);
        // 「かな」(2文字)と「かに」(2文字) → 1
        assert_eq!(levenshtein("かな", "かに"), 1);
    }

    #[test]
    fn asymmetric_length() {
        // 文字数が大きく異なる場合
        assert_eq!(levenshtein("abcdef", "a"), 5);
        assert_eq!(levenshtein("a", "abcdef"), 5);
    }

    #[test]
    fn distance_bound_by_max_len() {
        // 距離は max(len(a), len(b)) を超えない
        assert!(levenshtein("longer_string_here", "s") <= 17);
    }
}
