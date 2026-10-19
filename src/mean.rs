//! 単語の意味を取得する層
//!
//! 実装(OpenRouter等)は差し替え可能にするため `MeaningProvider` トレイトとして切り出す。
//! v3では OpenRouter API 直叩きが util.rs に埋め込まれており差し替えが困難だったため、
//! v4ではこのトレイト経由で利用する。
//!
//! - 全メソッドは非同期(`async`)
//! - `Box<dyn MeaningProvider + Send + Sync>` として注入してよい

use std::{future::Future, pin::Pin};

/// 単語の意味を提供するプロバイダ
///
/// トレイトオブジェクト(`Box<dyn MeaningProvider + Send + Sync>`)として
/// 注入できるよう、非同期メソッドは Boxed Future で定義している。
pub trait MeaningProvider: Send + Sync {
    /// 単語の意味を取得する。
    ///
    /// 取得できない場合(未対応・API失敗・タイムアウト等)は、
    /// 意味の代わりとして表示可能なエラーメッセージ文字列を返すこと。
    /// 処理は必ず非同期で行われ、失敗してもパニックしないこと。
    fn get_meaning<'a>(&'a self, word: String)
    -> Pin<Box<dyn Future<Output = String> + Send + 'a>>;

    /// 投稿前の進行中表示(v3の「検索中...」相当)。オーバーライド可。
    fn searching_text(&self) -> &str {
        "意味を検索中..."
    }
}

/// 実装未提供のデフォルトプロバイダ
///
/// 常に固定メッセージを返す。将来的に実装を追加する場合は
/// `MeaningProvider` を実装した型を作り、main.rs で注入する。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopMeaningProvider;

/// 意味検索が未実装であることを示す固定メッセージ
pub const MEANING_UNIMPLEMENTED_MESSAGE: &str = "現在は意味検索機能が実装されていません。";

impl MeaningProvider for NoopMeaningProvider {
    fn get_meaning<'a>(
        &'a self,
        _word: String,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        Box::pin(async move { MEANING_UNIMPLEMENTED_MESSAGE.to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::{MEANING_UNIMPLEMENTED_MESSAGE, MeaningProvider, NoopMeaningProvider};

    #[tokio::test]
    async fn noop_returns_fixed_message() {
        let provider = NoopMeaningProvider;
        let result = provider.get_meaning("apple".to_string()).await;
        assert_eq!(result, MEANING_UNIMPLEMENTED_MESSAGE);
    }

    #[tokio::test]
    async fn noop_is_async_and_blank_safe() {
        let provider = NoopMeaningProvider;
        // 空文字・マルチバイト・長文字列でもパニックしない
        assert_eq!(
            provider.get_meaning(String::new()).await,
            MEANING_UNIMPLEMENTED_MESSAGE
        );
        assert_eq!(
            provider
                .get_meaning("例え#あまりにも長い文字列".repeat(1000))
                .await,
            MEANING_UNIMPLEMENTED_MESSAGE
        );
    }

    #[tokio::test]
    async fn provider_is_object_safe_and_swappable() {
        // Box<dyn MeaningProvider + Send + Sync> として注入できること(差し替え可能設計)
        let provider: Box<dyn MeaningProvider + Send + Sync> = Box::new(NoopMeaningProvider);
        assert_eq!(
            provider.get_meaning("cat".to_string()).await,
            MEANING_UNIMPLEMENTED_MESSAGE
        );
        assert_eq!(provider.searching_text(), "意味を検索中...");
    }
}
