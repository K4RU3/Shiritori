#[macro_export]
macro_rules! impl_repo_error_partial_eq {
    ($name:ident { $($variant:ident),* $(,)? }) => {
        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                match (self, other) {
                    $(
                        ($name::$variant, $name::$variant) => true,
                    )*
                    ($name::Database(_), $name::Database(_)) => true,
                    ($name::Other(_), $name::Other(_)) => true,
                    _ => false,
                }
            }
        }
    };
}