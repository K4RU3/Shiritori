#[macro_export]
macro_rules! assert_with_fail {
    // メッセージあり
    ($cond:expr, $on_fail:expr, $($arg:tt)+) => {{
        if !$cond {
            use std::time::SystemTime;
            let now = SystemTime::now();
            eprintln!("Assertion failed: {}", stringify!($cond));
            eprintln!("  {}", format!($($arg)+));
            eprintln!("  at {:?}", now);

            // `$on_fail` を async block で包むことで、同期・非同期どちらもOKにする
            futures::executor::block_on(async {
                let _ = $on_fail;
            });

            panic!("assert_with_fail! failed: {}", stringify!($cond));
        }
    }};

    // メッセージなし
    ($cond:expr, $on_fail:expr) => {{
        if !$cond {
            use std::time::SystemTime;
            let now = SystemTime::now();
            eprintln!("Assertion failed: {}", stringify!($cond));
            eprintln!("  at {:?}", now);

            futures::executor::block_on(async {
                let _ = $on_fail;
            });

            panic!("assert_with_fail! failed: {}", stringify!($cond));
        }
    }};
}
