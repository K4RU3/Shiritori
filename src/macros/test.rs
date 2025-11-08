use std::fmt::Debug;

use crate::database::repository::RepoError;

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

#[macro_export]
macro_rules! assert_or_ok {
    ($res:expr, $msg:expr $(, $($arg:tt)+)?) => {{
        match &$res {
            Ok(_) => (),
            Err(e) => panic!(
                concat!($msg, "\nエラー内容: {:?}"),
                $($($arg)+,)? e
            ),
        }
    }};
}

pub fn assert_room_not_found<T: Debug + PartialEq + Eq>(result: Result<T, RepoError>) {
    assert_eq!(
        result,
        Err(RepoError::RoomNotFound),
        "存在しないルームへの操作で想定外の結果: {:?}",
        result
    );
}
