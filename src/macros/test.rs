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

#[macro_export]
macro_rules! define_test_guard {
    ($repo_ty:ty) => {
        use std::sync::Arc;

        struct TestGuard {
            repo: Arc<$repo_ty>,
            dumppath: String,
        }

        impl Drop for TestGuard {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    eprintln!("Test failed — dumping database...");

                    let dumppath = self.dumppath.clone();
                    let repo = self.repo.clone();

                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("failed to build runtime for dump");

                        rt.block_on(async move {
                            if let Err(e) = repo.db.dump_database(&dumppath).await {
                                eprintln!("Failed to dump database: {:?}", e);
                            } else {
                                eprintln!("Database dumped successfully to {}", dumppath);

                                let sqlite_path =
                                    format!("{}.sqlite", dumppath.trim_end_matches(".dump"));
                                let status = std::process::Command::new("sqlite3")
                                    .arg(&sqlite_path)
                                    .arg(format!(".read {}", dumppath))
                                    .status();

                                match status {
                                    Ok(s) if s.success() => {
                                        eprintln!(
                                            "Database file created successfully: {}",
                                            dumppath
                                        );
                                    }
                                    Ok(s) => {
                                        eprintln!(
                                            "sqlite3 exited with non-zero status: {} (dump at {})",
                                            s, dumppath
                                        );
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "Failed to invoke sqlite3: {:?} (dump at {})",
                                            e, dumppath
                                        );
                                    }
                                }
                            }
                        });
                    });
                }
            }
        }
    };
}
