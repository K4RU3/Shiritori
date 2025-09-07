#[macro_export]
macro_rules! arc_rwlock {
    ($val:expr) => {
        std::sync::Arc::new(tokio::sync::RwLock::new($val))
    };
}