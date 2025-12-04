#![allow(dead_code)]
use chrono::NaiveDateTime;
use rusqlite::Error as SqliteError;
use rusqlite::OptionalExtension;
use rusqlite::Row;
use tokio::task::JoinError;
use std::sync::Arc;
use thiserror::Error;

use crate::database::db::DatabaseError;
use crate::{
    database::{
        db::{DataBase, QueryExecutor},
        wrap_params::{i64_to_u64_bitwise},
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
    #[error("そのユーザーはすでに存在します(UserAlreadyExists")]
    UserAlreadyExists,
    #[error("ユーザーが存在しません(UserNotFound)")]
    UserNotFound,
    #[error("その単語はすでにそのルームに存在します(WordAlreadyExists)")]
    WordAlreadyExists,
    #[error("追加する単語がNULLに相当します(NullWord)")]
    NullWord,
    #[error("投票が作成されていません(VoteNotExists)")]
    VoteNotExists,
    #[error("投票ステータスが不正です(InvalidVoteState)")]
    InvalidVoteState,
    #[error("ユーザー順序が破損しています(BrokenChain)")]
    BrokenChain,
    #[error("キューの先頭ユーザーではありません(NotFirstUser)")]
    NotFirstUser,
    #[error("JoinError: {0}")]
    JoinError(#[from] JoinError),
    #[error("データベースエラー: {0}")]
    Database(#[from] SqliteError), // ← rusqliteのエラーを自動変換
    #[error("不明なエラー: {0}")]
    Other(#[from] anyhow::Error), // ← anyhowなど他のResultを受け取る
}

// Database/Otherを除くPartialEqの実装
impl_repo_error_partial_eq!(RepoError {
    RoomNotFound,
    RoomAlreadyExists,
    UserAlreadyExists,
    WordAlreadyExists,
    VoteNotExists,
    InvalidVoteState,
    NullWord,
    BrokenChain,
    NotFirstUser
});

#[derive(thiserror::Error, Debug)]
pub enum TxError {
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("repo error: {0}")]
    Repo(#[from] RepoError),
    #[error("join error: {0}")]
    JoinError(#[from] JoinError),
}

impl Eq for RepoError {}

pub type Result<T, E = RepoError> = core::result::Result<T, E>;

#[derive(PartialEq, Eq, Debug)]
pub struct Vote {
    pub room_id: u64,
    pub user_id: u64,
    pub word: Option<String>,
    pub good: Vec<u64>,
    pub bad: Vec<u64>,
    pub none: Vec<u64>,
    pub updated_at: Option<NaiveDateTime>
}

#[derive(Clone)]
pub struct Repository {
    db: Arc<DataBase>,
}

impl Repository {
    pub fn new(db: DataBase) -> anyhow::Result<Repository> {
        Ok(Self { db: Arc::new(db) })
    }

    /// repositoryにルームを作成します
    /// 
    /// エラー可能性: 
    /// RoomALreadyExists
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
        let count = db_to_repo!(result, {})?;
        Ok(count)
    }
    
    /// ユーザー登録
    pub async fn add_user(&self, user_id: u64, room_id: u64) -> Result<usize> {
        let result = self
            .db
            .execute("INSERT INTO room_members (room_id, user_id) VALUES(?1, ?2)", wrap_params!(room_id, user_id)).await;
        
        let success_count = db_to_repo!(result, {
            SQLITE_CONSTRAINT_PRIMARYKEY => RepoError::UserAlreadyExists,
        })?;
        
        Ok(success_count)
    }

    /// repositoryのルームに既出単語を追加します
    /// 
    /// エラー可能性: 
    /// WordAlreadyExists
    /// RoomNotFound
    /// NullWord
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

    /// repositoryに登録されているルームのリストを取得します
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

    /// ルームに登録された既出単語のリストを取得します
    /// 
    /// エラー可能性: 
    /// RoomNotFound
    /// JoinError
    pub async fn get_words(&self, room_id: u64) -> Result<Vec<String>> {
        let list_result = self.db.exclusive_transaction(move |tx| -> Result<Vec<String>, TxError> {
            let _room_result = tx
                .query_row(
                    "SELECT id FROM rooms WHERE id = ?1",
                    wrap_params!(room_id),
                    |f| f.get::<_, i64>(0),
                )
                .map_err(|_| RepoError::RoomNotFound)?;
            
            let mut stmt = tx
                .prepare("SELECT word FROM room_words WHERE room_id = ?1")?;

            let rows = stmt
                .query_map(wrap_params!(room_id), |row| row.get::<_,String>(0))?;

            let list = rows.collect::<Result<Vec<_>,_>>()?;

            Ok(list)
        }).await;
        
        match list_result {
            Ok(list) => Ok(list),
            Err(TxError::Repo(e)) => Err(e),
            Err(TxError::JoinError(e)) => Err(RepoError::from(e)),
            Err(TxError::Sqlite(e)) => Err(RepoError::from(e)), // TODO: 実際のエラーキャッチを考えてない
            Err(TxError::Database(e)) => db_to_repo!(Err(e), {}),
        }
    }

    /// ルームの投票状態を作成します
    /// 
    /// エラー可能性: 
    /// NotFirstUser
    /// RoomNotFound
    /// WordAlreadyExists
    pub async fn add_vote_state(&self, room_id: u64, user_id: u64, word: &str) -> Result<()> {
        let word = word.to_string();
        self.db.exclusive_transaction(move |tx| -> Result<()> {
            let current_user_result = tx
                .query_one(
                    "SELECT current_user_id FROM room_votes WHERE room_id = ?1",
                    wrap_params!(room_id),
                    |row| Ok(row_to_u64(row, 0).map_err(RepoError::from)))
                    .map_err(DatabaseError::from);
            
            match current_user_result {
                Ok(row_result) => {
                    let current_user_id = row_result?;
                    if user_id != current_user_id {
                        return Err(RepoError::NotFirstUser);
                    }
                }
                Err(DatabaseError::Sqlite(SqliteError::QueryReturnedNoRows)) => {/* チェックを無視して続行 */}
                Err(e) => return Err(db_to_repo!(Err(e), {})?)
            }
            
            let insert_result = tx.execute(
                "INSERT OR REPLACE INTO room_votes (room_id, current_user_id, word) VALUES(?1, ?2, ?3)",
                wrap_params!(room_id, user_id, word)
            )
            .map_err(DatabaseError::from);
            
            db_to_repo!(insert_result, {
                // TODO: room_id / user_id によるエラー可能性
                SQLITE_CONSTRAINT_FOREIGNKEY => RepoError::RoomNotFound,
                SQLITE_ABORT => RepoError::WordAlreadyExists,
                SQLITE_CONSTRAINT_TRIGGER => RepoError::WordAlreadyExists,
            })?;

            Ok(())
        }).await?;

        Ok(())
    }

    /// ルームの投票状態を取得します
    pub async fn get_vote_state(&self, room_id: u64) -> Result<Option<Vote>> {
        let vote_optional: Option<Vote> = self.db.exclusive_transaction(move |tx| -> Result<Option<Vote>> {
            // 基本投票取得
            let room_vote_optional= tx
                .query_row(
                    "SELECT room_id, current_user_id, word, updated_at FROM room_votes WHERE room_id = ?1",
                    wrap_params!(room_id),
                    |row| {
                        let room_id = row_to_u64(row, 0)?;
                        let current_user_id = row_to_u64(row, 1)?;
                        let word = row.get::<_, Option<String>>(2)?;
                        let updated_at = row.get::<_, String>(3)?;
                        
                        Ok((room_id, current_user_id, word, updated_at))
                    }
                )
                .optional()?;
            
            let (room_id, current_user_id, word, updated_at_str) = match room_vote_optional {
                Some(v) => v,
                None => return Ok(None)
            };
            let updated_at = NaiveDateTime::parse_from_str(&updated_at_str, "%Y-%m-%d %H:%M:%S").ok();
            
            // 投票状態取得
            let sql = "SELECT user_id FROM member_votes WHERE room_id = ?1 AND state = ?2";
            let mut stmt = tx.prepare(sql)?;
            
            let vote_list: Vec<Vec<u64>> = ["good", "bad", "none"]
                .iter()
                .map(|&state| {
                    let rows = stmt.query_map(wrap_params!(room_id, state), |row| {
                        row_to_u64(row, 0)
                    })
                    .map_err(RepoError::from)?;

                    let values = rows
                        .collect::<Result<Vec<_>, _>>()?;

                    Ok(values)
                })
                .collect::<Result<Vec<_>, RepoError>>()?;

            Ok(Some(Vote {
                room_id,
                user_id:
                current_user_id,
                word,
                good: vote_list.get(0).unwrap().to_vec(),
                bad: vote_list.get(1).unwrap().to_vec(),
                none: vote_list.get(2).unwrap().to_vec(),
                updated_at 
            }))
        }).await?;

        Ok(vote_optional)
    }

    /// 投票を行います
    ///
    /// エラー可能性
    /// RoomNotFound
    /// VoteNotExists
    /// InvalidVoteState
    pub async fn vote(&self, room_id: u64, user_id: u64, state: &str) -> Result<()> {
        let result = self.db.execute(
            "UPDATE room_members SET state = ?3 WHERE room_id = ?1 AND user_id = ?2",
            wrap_params![room_id as i64, user_id as i64, state],
        ).await;
        
        let _update_count = db_to_repo!(result, {
            // TODO: room_id / user_id エラー可能性
            SQLITE_CONSTRAINT_FOREIGNKEY => RepoError::RoomNotFound,
            SQLITE_CONSTRAINT_CHECK => RepoError::InvalidVoteState,
            SQLITE_CONSTRAINT_TRIGGER => RepoError::VoteNotExists,
        })?;
        
        Ok(())
    }

    pub async fn set_queue(&self, room_id: u64, queue: Vec<u64>) -> Result<(), RepoError> {
        let result = self.db.exclusive_transaction(move |tx| -> Result<(), RepoError> {
            // ユーザー存在確認
            let placeholders = queue.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            let users_check_sql = format!(
                "SELECT COUNT(*) FROM room_members WHERE room_id = ?1 AND user_id IN ({})",
                placeholders
            );
            let found_count = {
                let mut stmt = tx.prepare(&users_check_sql)?;
                let mut params: Vec<&dyn rusqlite::ToSql> = vec![&room_id];
                for user_id in &queue {
                    params.push(user_id);
                }
                stmt.query_row(rusqlite::params_from_iter(params), |row| row.get::<_, i64>(0))?
            };
            if found_count != queue.len() as i64 {
                return Err(RepoError::UserNotFound);
            }

            // 既存のリンクをクリア
            let clear_result = tx.execute(
                "UPDATE room_members SET prev = NULL, next = NULL WHERE room_id = ?1",
                wrap_params![room_id],
            );
            
            if queue.len() == 0 {
                // 次のユーザー未定義
                return Ok(());
            }

            for i in 0..queue.len() {
                let user_id = queue[i];
                let prev_user_id = if i == 0 { queue[queue.len() - 1] } else { queue[i - 1] };
                let next_user_id = if i + 1 == queue.len() { queue[0] } else { queue[i + 1] };

                let update_result = tx.execute(
                    "UPDATE room_members SET prev = ?2, next = ?3 WHERE room_id = ?1 AND user_id = ?4",
                    wrap_params![
                        room_id,
                        prev_user_id,
                        next_user_id,
                        user_id
                    ],
                )?;
            }

            Ok(())
        }).await;
        
        Ok(())
    }

    /*
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
    use core::panic;
    use std::{fs, result};

    use crate::{
        assert_or_ok,
        database::{
            db::DataBase,
            repository::{RepoError, Repository},
        },
        define_test_guard,
    };
    use anyhow::Result;

    define_test_guard!(Repository);

    // セットアップ
    async fn setup_repo() -> Result<Repository> {
        let init_sql = fs::read_to_string("./schema.sql")?;
        let db = DataBase::new(":memory:", Some(&init_sql)).await?;
        Repository::new(db)
    }

    async fn setup_create_rooms(repo: &Repository, rooms: &Vec<u64>) {
        for id in rooms {
            repo.create_room(*id).await.unwrap_or_else(|e| {
                panic!(
                    "テスト対象外のcreate_roomでエラーが発生しました。\nエラー: {:?}",
                    e
                )
            });
        }
    }
    
    async fn setup_add_users(repo: &Repository, users: &Vec<u64>, room_id: u64) {
        for id in users {
            repo.add_user(*id, room_id).await.unwrap_or_else(|e| {
                panic!(
                    "テスト対象外のadd_userでエラーが発生しました。\nエラー: {:?}",
                    e
                )
            });
        }
    }

    async fn setup_delete_rooms(repo: &Repository, rooms: &Vec<u64>) {
        for id in rooms {
            repo.delete_room(*id).await.unwrap_or_else(|e| {
                panic!(
                    "テスト対象外のdelete_roomでエラーが発生しました。\nエラー: {:?}",
                    e
                )
            });
        }
    }

    async fn setup_insert_words(repo: &Repository, room_id: u64, words: &Vec<&str>) {
        for word in words {
            repo.insert_word(room_id, word).await.unwrap_or_else(|e| {
                panic!(
                    "テスト対象外のinsert_wordでエラーが発生しました。\nエラー: {:?}",
                    e
                )
            });
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
            let result = repo.create_room(1).await;
            assert_or_ok!(result, "1つ目のルーム作成時にエラーが発生しました。");
        }

        // 他のルーム作成に問題がない
        {
            let result = repo.create_room(2).await;
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
            let delete_count = repo.delete_room(1).await.unwrap_or_else(|e| {
                panic!(
                    "適切なルームの削除でエラーが発生しました。\nエラー: {:?}",
                    e
                )
            });
            assert_ne!(delete_count, 0, "削除されたルームの数が0件でした。");

            // 再登録の確認
            let recreate = repo.create_room(1).await;
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
            assert_eq!(
                result,
                Err(RepoError::RoomNotFound),
                "未作成のルームからの単語取得で、想定されていない処理がされました。\nresult: {:?}",
                result.as_ref()
            )
        }

        setup_create_rooms(&repo, &vec![1]).await;

        // 作成後空取得テスト
        {
            let result = repo.get_words(1).await;
            assert_eq!(
                result,
                Ok(Vec::<String>::new()),
                "単語情報のないルームからの単語取得において、空の配列が返りませんでした。\nresult: {:?}",
                result.as_ref()
            );
        }

        setup_insert_words(&repo, 1, &word_list).await;

        // 単語挿入後テスト
        {
            let result = repo.get_words(1).await;
            assert_or_ok!(result, "正常な単語軍の取得でエラーが発生しました。");
            let mut original = word_list.clone(); let mut target = result.unwrap(); original.sort();
            target.sort();
            assert_eq!(
                original, target,
                "挿入した単語群と取得した単語群が異なります。\noriginal: {:?}\ntarget: {:?}",
                original, target
            );
        }

        setup_delete_rooms(&repo, &vec![1]).await;
        setup_create_rooms(&repo, &vec![1]).await;
        // ルーム削除後取得テスト
        {
            let result = repo.get_words(1).await;
            assert_eq!(
                result,
                Ok(Vec::<String>::new()),
                "再作成されたルームからからのリストが返りませんでした。\nresult: {:?}",
                result.as_ref()
            );
        }

        Ok(())
    }
    
    #[tokio::test]
    async fn test_vote() -> Result<()> {
        let repo = setup_repo().await?;
        let _guard = TestGuard {
            repo: Arc::new(repo.clone()),
            dumppath: "dump/dump_vote_test.db".into(),
        };
        
        // ルーム未作成テスト
        {
            let result = repo.get_vote_state(1).await;
            assert_eq!(result, Ok(None), "ルーム未生成時に空であるべき投票の取得に問題が発生しました。\nresult: {:?}", result);
        }
        
        setup_create_rooms(&repo, &vec![1]).await;
        setup_add_users(&repo, &vec![100,101,102,103,104], 1).await;

        // 投票未作成テスト
        {
            let result = repo.get_vote_state(1).await;
            assert_eq!(result, Ok(None), "初期化時に空であるべき投票の取得に問題が発生しました。\nresult: {:?}", result);
        }
        
        // 未作成投票エラーテスト
        {
            let result = repo.vote(1, 100, "good".into()).await;
            assert_eq!(result, Err(RepoError::VoteNotExists), "投票未作成時にVoteNotExistsエラー以外が返されました。\nresult: {:?}", result);
        }
        
        // 投票作成
        {
            let result = repo.add_vote_state(1, 100, "test".into()).await;
            assert_or_ok!(result, "正常な投票の作成でエラーが発生しました。");
        }
        
        // 投票
        {
            let result_good = repo.vote(1, 101, "good".into()).await;
            assert_or_ok!(result_good, "goodの投票に失敗しました。");
            let result_bad = repo.vote(1, 101, "bad".into()).await;
            assert_or_ok!(result_bad, "badの投票に失敗しました。");
            let result_none = repo.vote(1, 101, "none".into()).await;
            assert_or_ok!(result_none, "noneの投票に失敗しました。");
        }
        
        // 不正投票
        {
            let result_null_state = repo.vote(1, 102, "".into()).await;
            assert_eq!(result_null_state, Err(RepoError::InvalidVoteState), "空文字列の投票でInvalidVoteState以外のエラーが発生しました。\nresult: {:?}", result_null_state);
            let result_invalid_state = repo.vote(1, 103, "invalid".into()).await;
            assert_eq!(result_invalid_state, Err(RepoError::InvalidVoteState), "不正な投票でInvalidVoteState以外のエラーが発生しました。\nresult: {:?}", result_invalid_state);
        }
        
        Ok(())
    }

    /*
    #[tokio::test]
    async fn test_initial_repository() -> Result<()> {
        let repo = setup_repo().await?;

        Ok(())
    }
     */
}

fn row_to_u64(row: &Row<'_>, idx: usize) -> Result<u64, SqliteError> {
    let i: i64 = row.get(idx)?;
    Ok(i64_to_u64_bitwise(i))
}
