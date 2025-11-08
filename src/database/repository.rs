#![allow(dead_code)]
use rusqlite::Error as SqliteError;
use rusqlite::Row;
use std::sync::Arc;
use thiserror::Error;

use crate::{
    database::{
        db::DataBase,
        wrap_params::{i64_to_u64_bitwise, u64_to_i64_bitwise},
    },
    wrap_params,
};
use crate::{db_to_repo, impl_repo_error_partial_eq};

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("ルームが存在しません(RoomNotFound)")]
    RoomNotFound,
    #[error("そのルームはすでに存在します(RoomAlreadyExists)")]
    RoomAlreadyExists,
    #[error("その単語はすでにそのルームに存在します(WordAlreadyExists)")]
    WordAlreadyExists,
    #[error("追加する単語がNULLに相当します(NullWord)")]
    NullWord,
    #[error("ユーザー順序が破損しています(BrokenChain)")]
    BrokenChain,
    #[error("データベースエラー: {0}")]
    Database(#[from] SqliteError), // ← rusqliteのエラーを自動変換
    #[error("不明なエラー: {0}")]
    Other(#[from] anyhow::Error), // ← anyhowなど他のResultを受け取る
}

// Database/Otherを除くPartialEqの実装
impl_repo_error_partial_eq!(RepoError {
    RoomNotFound,
    RoomAlreadyExists,
    WordAlreadyExists,
    NullWord,
    BrokenChain
});

impl Eq for RepoError {}

pub type Result<T, E = RepoError> = core::result::Result<T, E>;

pub struct Vote {
    pub user_id: u64,
    pub word: Option<String>,
    pub good: Vec<u64>,
    pub bad: Vec<u64>,
    pub none: Vec<u64>,
}

#[derive(Clone)]
pub struct Repository {
    db: Arc<DataBase>,
}

impl Repository {
    pub fn new(db: DataBase) -> anyhow::Result<Repository> {
        Ok(Self { db: Arc::new(db) })
    }

    pub async fn create_room(&self, room_id: u64) -> Result<()> {
        let result = self
            .db
            .execute("INSERT INTO rooms VALUES(?1)", wrap_params!(room_id))
            .await;
        db_to_repo!(result, {
            SQLITE_CONSTRAINT_PRIMARYKEY => RepoError::RoomAlreadyExists,
        })?;
        Ok(())
    }

    /// 引数に与えられたルームを削除し、削除件数を返します。
    pub async fn delete_room(&self, room_id: u64) -> Result<usize> {
        let result = self
            .db
            .execute("DELETE FROM rooms WHERE id = (?1)", wrap_params!(room_id))
            .await;

        // Okにより削除件数が返されます。
        // 基本的に特有エラーはなし
        Ok(db_to_repo!(result, {})?)
    }

    pub async fn insert_word(&self, room_id: u64, word: &str) -> Result<usize> {
        if word.len() == 0 {
            return Err(RepoError::NullWord);
        }

        let result = self
            .db
            .execute(
                "INSERT INTO room_words VALUES(?1, ?2)",
                wrap_params!(room_id, word),
            )
            .await;

        let inserted_count = db_to_repo!(result, {
            SQLITE_CONSTRAINT_PRIMARYKEY => RepoError::WordAlreadyExists,
            SQLITE_CONSTRAINT_FOREIGNKEY => RepoError::RoomNotFound,
            SQLITE_CONSTRAINT_NOTNULL => RepoError::NullWord
        })?;

        Ok(inserted_count)
    }

    pub async fn get_rooms(&self) -> Result<Vec<u64>> {
        let result = self
            .db
            .query_map("SELECT id FROM rooms", [], |row| {
                let room_id_i64: i64 = row.get(0)?;
                Ok(i64_to_u64_bitwise(room_id_i64))
            })
            .await;

        let list = db_to_repo!(result, {})?;

        Ok(list)
    }

    pub async fn get_words(&self, room_id: u64) -> Result<Vec<String>> {
        let room_result = self.db.query_row("SELECT id FROM rooms WHERE id = ?1", wrap_params!(room_id), |f|f.get::<_,i64>(0)).await;
        if room_result.is_err() {
            return Err(RepoError::RoomNotFound);
        }

        let result = self
            .db
            .query_map(
                "SELECT word FROM room_words WHERE room_id = ?1",
                wrap_params!(room_id),
                |row| {
                    let str = row.get::<_, String>(0)?;
                    
                    Ok(str)
                },
            )
            .await;
        
        let list = db_to_repo!(result, {})?;
        
        Ok(list)
    }

    /*
    pub async fn add_vote_state(&self, room_id: u64, user_id: u64, word: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO room_votes (room_id, current_user_id, word) VALUES (?1, ?2, ?3)",
            wrap_params![room_id, user_id, word],
        ).await?;
        Ok(())
    }

    pub async fn get_vote_state(&self, room_id: u64) -> Result<Option<Vote>> {
        let result: Option<Vote> = self.db.exclusive_transaction(move |tx| {
            // ルームの存在確認
            let room_exists = tx.query_row(
                "SELECT id FROM rooms WHERE id = (?1)",
                [room_id],
                |_row| Ok(()),
            );

            if room_exists.is_err() {
                // トランザクション内のErrは即座に伝播
                return Err(RepoError::RoomNotFound.into());
            }

            // vote情報を取得
            let vote_row = tx.query_row(
                "SELECT current_user_id, word FROM room_votes WHERE room_id = (?1)",
                [room_id],
                |row| {
                    let user_id = i64_to_u64_bitwise(row.get(0)?);
                    let word = row.get::<_, Option<String>>(1)?;
                    Ok((user_id, word))
                },
            );

            // 該当データがなければ None
            let (user_id, word) = match vote_row {
                Ok(res) => res,
                Err(_) => return Ok(None),
            };

            // 投票状態取得
            let sql = "SELECT user_id FROM member_votes WHERE room_id = ?1 AND state = ?2";

            // good
            let mut stmt = tx.prepare(sql)?;
            let good_iter = stmt.query_map(wrap_params!(room_id, "good"), |row| {
                let id: i64 = row.get(0)?;
                Ok(i64_to_u64_bitwise(id))
            })?;
            let good: Vec<u64> = good_iter.filter_map(Result::ok).collect();

            // bad
            let bad_iter = stmt.query_map(wrap_params!(room_id, "bad"), |row| {
                let id: i64 = row.get(0)?;
                Ok(i64_to_u64_bitwise(id))
            })?;
            let bad: Vec<u64> = bad_iter.filter_map(Result::ok).collect();

            // none
            let none_iter = stmt.query_map(wrap_params!(room_id, "none"), |row| {
                let id: i64 = row.get(0)?;
                Ok(i64_to_u64_bitwise(id))
            })?;
            let none: Vec<u64> = none_iter.filter_map(Result::ok).collect();

            let vote = Vote {
                user_id,
                word,
                good,
                bad,
                none,
            };

            Ok(Some(vote))
        })
        .await?;

        Ok(result)
    }

    pub async fn vote(&self, room_id: u64, user_id: u64, state: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO member_votes (room_id, user_id, state)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(room_id, user_id) DO UPDATE SET state = excluded.state",
            wrap_params![room_id as i64, user_id as i64, state],
        ).await?;
        Ok(())
    }

    pub async fn delete_vote(&self, room_id: u64) -> Result<()> {
        self.db.exclusive_transaction(move |tx| {
            tx.execute(
                "DELETE FROM room_votes WHERE room_id = ?1",
                [room_id as i64],
            )?;
            tx.execute(
                "UPDATE member_votes SET state = 'none' WHERE room_id = ?1",
                [room_id as i64],
            )?;
            Ok(())
        }).await?;
        Ok(())
    }

    pub async fn set_queue(&self, room_id: u64, queue: Vec<u64>) -> Result<(), RepoError> {
        let queue_clone = queue.clone();
        self.db.exclusive_transaction(move |tx| {
            match tx.query_row(
                "SELECT id FROM rooms WHERE id = ?1",
                [room_id],
                |_row| Ok(())
            ) {
                Ok(()) => Ok(()),
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(RepoError::RoomNotFound),
                Err(e) => Err(RepoError::Database(e.into())),
            }?;


            // 全ルーム内ユーザーをいったん削除
            tx.execute("UPDATE room_members SET prev = NULL, next = NULL WHERE room_id = ?1", [room_id])?;
            tx.execute("DELETE FROM room_members WHERE room_id = ?1", [room_id])?;

            // ユーザーがいなければ終了
            if queue.is_empty() { return Ok(()); }

            for user in queue {
                tx.execute("INSERT OR IGNORE INTO users VALUES(?1)", [user])?;
                tx.execute("INSERT INTO room_members VALUES(?1, ?2, NULL, NULL)", [room_id, user])?;
            }

            for (i, &user) in queue_clone.iter().enumerate() {
                let prev = if i == 0 { *queue_clone.last().unwrap() } else { *queue_clone.get(i - 1).unwrap() };
                let next = if i == queue_clone.len() - 1 { *queue_clone.get(0).unwrap() } else { *queue_clone.get(i + 1).unwrap() };
                tx.execute("UPDATE room_members SET prev = ?1 WHERE room_id = ?2 AND user_id = ?3", [prev, room_id, user])?;
                tx.execute("UPDATE room_members SET next = ?1 WHERE room_id = ?2 AND user_id = ?3", [next, room_id, user])?;
            }

            tx.execute("DELETE FROM room_votes WHERE room_id = ?1", [room_id])?;
            tx.execute("INSERT INTO room_votes (room_id, current_user_id, word) VALUES (?1, ?2, NULL)", wrap_params![room_id, *queue_clone.get(0).unwrap()])?;

            Ok(())
        }).await?;

        Ok(())
    }

    pub async fn get_queue(&self, room_id: u64) -> Result<Vec<u64>> {
        self.db
            .exclusive_transaction(move |tx| {
                // ルームがないときは空配列
                if let Err(_) = tx.query_row("SELECT id FROM rooms WHERE id = ?1", [room_id], |_r| Ok(())) {
                    return Ok(Vec::<u64>::new());
                }

                let current_user= match tx.query_row(
                    "SELECT current_user_id FROM room_votes WHERE room_id = ?1",
                    [room_id],
                    |row| {
                        let i: i64 = row.get(0)?;
                        Ok(i64_to_u64_bitwise(i))
                    },
                ) {
                    Ok(id) => id,
                    Err(rusqlite::Error::QueryReturnedNoRows) => { return Ok(Vec::<u64>::new()); },
                    Err(e) => return Err(e.into()),
                };


                let mut stmt = tx.prepare(
                    "SELECT user_id, prev, next FROM room_members WHERE room_id = ?1",
                )?;
                let rows = stmt.query_map([room_id], |row| {
                    let i: i64 = row.get(0)?;
                    let u0 = i64_to_u64_bitwise(i);

                    Ok((
                        u0,
                        row.get::<_, Option<i64>>(1)?.map(i64_to_u64_bitwise),
                        row.get::<_, Option<i64>>(2)?.map(i64_to_u64_bitwise),
                    ))
                }).map_err(|_e| RepoError::BrokenChain)?;


                use std::collections::{HashMap, HashSet};
                let mut links: HashMap<u64, (Option<u64>, Option<u64>)> = HashMap::new();

                for row in rows {
                    let (user_id, prev, next) = row?;
                    links.insert(user_id, (prev, next));
                }

                let mut queue = Vec::new();
                let mut visited = HashSet::new();
                let mut cursor = Some(current_user);

                while let Some(user_id) = cursor {
                    // すでに訪問済みなら循環 → 終了
                    if !visited.insert(user_id) {
                        break;
                    }

                    queue.push(user_id);

                    // 次のユーザーへ進む
                    if let Some((_, next)) = links.get(&user_id) {
                        cursor = *next;
                    }
                }

                if queue.len() < links.len() {
                    return Err(RepoError::BrokenChain.into());
                }

                Ok(queue)
            })
            .await
            .map_err(Into::into)
    }


    pub async fn next_user(&self, room_id: u64) -> Result<()> {
        self.db.exclusive_transaction(move |tx| {
            // 現在のユーザーを取得
            let current: Option<i64> = match tx.query_row(
                "SELECT current_user_id FROM room_votes WHERE room_id = ?1",
                [room_id as i64],
                |row| row.get(0),
            ) {
                Ok(v) => Some(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            };

            // 現在ユーザーが存在しない場合は何もしない
            let Some(current_user_id) = current else {
                return Ok(());
            };

            // 次のユーザーを取得
            let next_user: Option<i64> = match tx.query_row(
                "SELECT next FROM room_members WHERE room_id = ?1 AND user_id = ?2",
                [room_id as i64, current_user_id],
                |row| row.get(0),
            ) {
                Ok(v) => Some(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            };

            // 次ユーザーがNULLでもそのまま反映
            tx.execute(
                "UPDATE room_votes SET current_user_id = ?2 WHERE room_id = ?1",
                wrap_params![room_id as i64, next_user.unwrap()],
            )?;

            Ok(())
        }).await?;

        Ok(())
    }

    pub async fn dump_database<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        self.db.dump_database(path).await
    }

    */
}



#[cfg(test)]
mod tests {
    use std::fs;

    use crate::{assert_or_ok, database::{
        db::DataBase,
        repository::{RepoError, Repository},
    }};
    use anyhow::Result;

    struct TestGuard<'a> {
        repo: &'a Repository,
        dumppath: String
    }


    impl<'a> Drop for TestGuard<'a> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                eprintln!("Test failed — dumping database...");

                // dumpパスと参照をクローン（スレッドにmoveできるように）
                let dumppath = self.dumppath.clone();
                // repoはDrop中にmoveできないので、Arcなどで共有できるようにしておく必要あり
                let repo = (*self.repo).clone();

                // 別スレッドで非同期ダンプ実行
                std::thread::spawn(move || {
                    // このスレッド専用のRuntimeを作る（既存Tokioとは独立）
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("failed to build runtime for dump");

                    // 非同期関数をブロッキングで実行
                    rt.block_on(async move {
                        if let Err(e) = repo.db.dump_database(&dumppath).await {
                            eprintln!("Failed to dump database: {:?}", e);
                        } else {
                            eprintln!("Database dumped successfully to {}", dumppath);

                            // sqlite3コマンドで .sqlite に復元
                            let sqlite_path = format!("{}.sqlite", dumppath.trim_end_matches(".dump"));
                            let status = std::process::Command::new("sqlite3")
                                .arg(&sqlite_path)                     // 生成するDBファイル
                                .arg(format!(".read {}", dumppath))    // 読み込むSQLダンプ
                                .status();

                            match status {
                                Ok(s) if s.success() => {
                                    eprintln!("Database file created successfully: {}", dumppath);
                                }
                                Ok(s) => {
                                    eprintln!(
                                        "sqlite3 exited with non-zero status: {} (dump at {})",
                                        s, dumppath
                                    );
                                }
                                Err(e) => {
                                    eprintln!("Failed to invoke sqlite3: {:?} (dump at {})", e, dumppath);
                                }
                            }
                        }
                    });
                });
            }
        }
    }
    
    // セットアップ
    async fn setup_repo() -> Result<Repository> {
        let init_sql = fs::read_to_string("./schema.sql")?;
        let db = DataBase::new(":memory:", Some(&init_sql)).await?;
        Repository::new(db)
    }
    
    async fn setup_create_rooms(repo: &Repository, rooms: &Vec<u64>) {
        for id in rooms {
            repo.create_room(*id)
                .await
                .unwrap_or_else(|e| panic!("テスト対象外のcreate_roomでエラーが発生しました。\nエラー: {:?}", e));
        }
    }
    
    async fn setup_insert_words(repo: &Repository, room_id: u64, words: &Vec<&str>) {
        for word in words {
            repo.insert_word(room_id, word)
                .await
                .unwrap_or_else(|e| panic!("テスト対象外のinsert_wordでエラーが発生しました。\nエラー: {:?}", e));
        }
    }

    // 初期化テスト

    #[tokio::test]
    async fn test_initial_repository() -> Result<()> {
        let repo = setup_repo().await;
        assert_or_ok!(repo, "単純なRepositoryの初期化でエラーが発生しました。");
        
        Ok(())
    }


    // ルーム操作テスト

    #[tokio::test]
    async fn test_create_room() -> Result<()> {
        let repo = setup_repo().await?;

        // 作成に問題がない
        {
            let result = repo
                .create_room(1)
                .await;
            assert_or_ok!(result, "1つ目のルーム作成時にエラーが発生しました。");
        }

        // 他のルーム作成に問題がない
        {
            let result = repo
                .create_room(2)
                .await;
            assert_or_ok!(result, "2つ目のルーム作成時にエラーが発生しました。");
        }

        // 重複するルームの作成
        {
            let result = repo.create_room(1).await;
            assert_eq!(
                result,
                Err(RepoError::RoomAlreadyExists),
                "重複するルームの作成で適切なエラーが発生しませんでした。\n内容: {:?}",
                result
            );
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_delete_room() -> Result<()> {
        let repo = setup_repo().await?;
        let _ = setup_create_rooms(&repo, &vec![1]).await;

        // 削除の確認
        {
            // 削除処理
            let delete_count = repo
                .delete_room(1)
                .await
                .unwrap_or_else(|e| panic!("適切なルームの削除でエラーが発生しました。\nエラー: {:?}", e));
            assert_ne!(delete_count, 0, "削除されたルームの数が0件でした。");

            // 再登録の確認
            let recreate = repo
                .create_room(1)
                .await;
            assert_or_ok!(recreate, "ルームの削除後の作成に失敗しました。");
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_get_rooms() -> Result<()> {
        let repo = setup_repo().await?;
        let room_list: Vec<u64> = vec![1, 3, 6, 8]; // ソート前提

        // 初期化時テスト
        {
            let result = repo.get_rooms().await;
            assert_eq!(
                result,
                Ok(Vec::<u64>::new()),
                "初期化時に空であるルームリストが空ではありませんでした。"
            );
        }
        
        setup_create_rooms(&repo, &room_list).await; // ルーム追加

        // 追加時テスト
        {
            let result = repo.get_rooms().await;
            assert!(
                result.is_ok(),
                "ルームの取得で不明のエラーが発生しました。\nエラー: {:?}",
                result.unwrap_err()
            );
            let mut got_room_list = result.unwrap();
            got_room_list.sort();
            assert_eq!(
                got_room_list, room_list,
                "追加したルームに不整合が発生しました。\n{:?} != {:?}",
                got_room_list, room_list
            );
        }

        Ok(())
    }

    // 単語操作テスト

    #[tokio::test]
    async fn test_insert_word() -> Result<()> {
        let repo = setup_repo().await?;

        // ルーム未作成時挿入
        {
            let result = repo.insert_word(1, "test").await;
            assert_eq!(
                result,
                Err(RepoError::RoomNotFound),
                "存在しないルームへの単語挿入時、想定されない処理がされました。\nresult: {:?}",
                result
            );
        }

        setup_create_rooms(&repo, &vec![1]).await;

        // ルームに挿入
        {
            let result = repo.insert_word(1, "apple").await;
            assert_eq!(
                result,
                Ok(1),
                "標準的な単語の追加に失敗しました。\nエラー: {:?}",
                result.as_ref().err()
            );
        }

        // 同一ワード挿入
        {
            let result = repo.insert_word(1, "apple").await;
            assert_eq!(
                result,
                Err(RepoError::WordAlreadyExists),
                "すでに存在する単語の挿入で想定されていない処理がされました。\nresult: {:?}",
                result
            );
        }

        // ルーム削除時
        {
            let _ = repo.delete_room(1).await?;
            let _ = repo.create_room(1).await?;
            let result = repo.insert_word(1, "apple").await;
            assert_eq!(
                result,
                Ok(1),
                "再生成後のルームへの単語挿入でエラーが発生しました。\nエラー: {:?}",
                result.as_ref().err()
            );
        }

        //  NULLに準ずるワード挿入
        {
            let result = repo.insert_word(1, "").await;
            assert_eq!(
                result,
                Err(RepoError::NullWord),
                "Nullに準ずるワードの挿入で想定されていない処理がされました。\nエラー: {:?}",
                result
            );
        }

        Ok(())
    }
    
    #[tokio::test]
    async fn test_get_words() -> Result<()> {
        let repo = setup_repo().await?;
        let word_list: Vec<&str> = vec!["apple", "banana", "god"];
        
        // ルーム未作成時テスト
        {
            let result = repo.get_words(1).await;
            assert_eq!(result, Err(RepoError::RoomNotFound), "未作成のルームからの単語取得で、想定されていない処理がされました。\nresult: {:?}", result.as_ref())
        }
        
        setup_create_rooms(&repo, &vec![1]).await;
        
        // 作成後空取得テスト
        {
            let result = repo.get_words(1).await;
            assert_eq!(result, Ok(Vec::<String>::new()), "単語情報のないルームからの単語取得において、空の配列が返りませんでした。\nresult: {:?}", result.as_ref());
        }
        
        setup_insert_words(&repo, 1, &word_list).await;
        
        // 単語挿入後テスト
        {
            let result = repo.get_words(1).await;
            assert_or_ok!(result, "正常な単語軍の取得でエラーが発生しました。");
            let mut original = word_list.clone();
            let mut target = result.unwrap();
            original.sort();
            target.sort();
            assert_eq!(original, target, "挿入した単語群と取得した単語群が異なります。\noriginal: {:?}\ntarget: {:?}", original, target);
        }

        Ok(())
    }
}


fn row_to_u64(row: &Row<'_>, idx: usize) -> Result<u64> {
    let i: i64 = row.get(idx)?;
    Ok(i64_to_u64_bitwise(i))
}
