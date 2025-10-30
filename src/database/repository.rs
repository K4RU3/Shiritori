use anyhow::Result;
use thiserror::Error;
use std::{sync::Arc};

use crate::database::db::{self, DataBase};

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("ルームが存在しません(RoomNotFound)")]
    RoomNotFound,
    #[error("ユーザー順序が破損しています(BrokenChain)")]
    BrokenChain,
    #[error("データベースエラー: {0}")]
    Database(#[from] rusqlite::Error), // ← rusqliteのエラーを自動変換
    #[error("不明なエラー: {0}")]
    Other(#[from] anyhow::Error),      // ← anyhowなど他のResultを受け取る
}

pub struct Repository {
    db: Arc<DataBase>,
}

impl Repository {
    pub fn new(db_path: &str) -> anyhow::Result<Repository> {
        let database = DataBase::new(db_path)?;
        Ok(Self {
            db: Arc::new(database),
        })
    }

    pub async fn create_room(&self, room_id: u64) -> Result<()> {
        self.db.execute("INSERT INTO rooms VALUES(?1)", [room_id])?;
        Ok(())
    }

    pub async fn delete_room(&self, room_id: u64) -> Result<()> {
        self.db.execute("DELETE FROM rooms WHERE id = ?1", [room_id])?;
        Ok(())
    }

    pub async fn insert_word(&self, room_id: u64, word: &str) -> Result<()> {
        self.db.execute("INSERT INTO room_words VALUES(?1, ?2)", rusqlite::params![room_id, word])?;
        Ok(())
    }

    pub async fn get_rooms(&self) -> Result<Vec<u64>> {
        Ok(self
            .db
            .query_map("SELECT room_id FROM rooms", [], |row| row.get::<_, u64>(0))?)
    }

    pub async fn get_words(&self, room_id: u64) -> Result<Vec<String>> {
        Ok(self.db.query_map(
            "SELECT words FROM room_words WHERE room_id = ?1",
            [room_id],
            |row| row.get(0),
        )?)
    }

    pub async fn add_vote_state(&self, room_id: u64, user_id: u64, word: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO room_votes (room_id, current_user_id, word) VALUES (?1, ?2, ?3)",
            rusqlite::params![room_id as i64, user_id as i64, word],
        )?;
        Ok(())
    }

    pub async fn vote(&self, room_id: u64, user_id: u64, state: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO member_votes (room_id, user_id, state)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(room_id, user_id) DO UPDATE SET state = excluded.state",
            rusqlite::params![room_id as i64, user_id as i64, state],
        )?;
        Ok(())
    }

    pub async fn delete_vote(&self, room_id: u64) -> Result<()> {
        self.db.exclusive_transaction(|tx| {
            tx.execute(
                "DELETE FROM room_votes WHERE room_id = ?1",
                [room_id as i64],
            )?;
            tx.execute(
                "UPDATE member_votes SET state = 'none' WHERE room_id = ?1",
                [room_id as i64],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub async fn set_queue(&self, room_id: u64, queue: Vec<u64>) -> Result<(), RepoError> {
        let queue_clone = queue.clone();
        self.db.exclusive_transaction(|tx| {
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
            tx.execute("INSERT INTO room_votes (room_id, current_user_id, word) VALUES (?1, ?2, ?3)", rusqlite::params![room_id, queue_clone.get(0), rusqlite::types::Null])?;

            Ok(())
        })?;

        Ok(())
    }

    pub async fn get_queue(&self, room_id: u64) -> Result<Vec<u64>> {
        self.db
            .exclusive_transaction(|tx| {
                // ルームがないときは空配列
                if let Err(_) = tx.query_row("SELECT id FROM rooms WHERE id = ?1", [room_id], |_r| Ok(())) {
                    return Ok(Vec::<u64>::new());
                }

                let current_user= match tx.query_row(
                    "SELECT current_user_id FROM room_votes WHERE room_id = ?1",
                    [room_id],
                    |row| row.get::<_, u64>(0),
                ) {
                    Ok(id) => id,
                    Err(rusqlite::Error::QueryReturnedNoRows) => { return Ok(Vec::<u64>::new()); },
                    Err(e) => return Err(e.into()),
                };


                let mut stmt = tx.prepare(
                    "SELECT user_id, prev, next FROM room_members WHERE room_id = ?1",
                )?;
                let rows = stmt.query_map([room_id], |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, Option<u64>>(1)?,
                        row.get::<_, Option<u64>>(2)?,
                    ))
                }).map_err(|e| RepoError::BrokenChain)?;


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
            .map_err(Into::into)
    }


    pub async fn next_user(&self, room_id: u64) -> Result<()> {
        self.db.exclusive_transaction(|tx| {
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
                rusqlite::params![room_id as i64, next_user],
            )?;

            Ok(())
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use rusqlite::params;
    use std::sync::Arc;

    /// --- 共通初期化: テスト用Repository ---
    fn setup_repo() -> Result<Repository> {
        let db = DataBase::new(":memory:")?;
        let repo = Repository { db: Arc::new(db) };

        // スキーマ読み込み（プロジェクトルートのschema.sqlを流用）
        repo.db.load_schema_from_file("schema.sql")?;
        Ok(repo)
    }

    /// --- 共通初期データ登録 ---
    fn init_test_data(repo: &Repository) -> Result<()> {
        repo.db.exclusive_transaction(|tx| {
            let room_id: i64 = 1;
            let user1: i64 = 2;
            let user2: i64 = 3;
            let nul = rusqlite::types::Null;

            // rooms
            tx.execute("INSERT INTO rooms VALUES(?1)", [room_id])?;
            // users
            tx.execute("INSERT INTO users VALUES(?1)", [user1])?;
            tx.execute("INSERT INTO users VALUES(?1)", [user2])?;

            // room_members（まずはnext/prevをNULLで登録）
            tx.execute(
                "INSERT INTO room_members VALUES(?1, ?2, ?3, ?4)",
                params![room_id, user1, nul, nul],
            )?;
            tx.execute(
                "INSERT INTO room_members VALUES(?1, ?2, ?3, ?4)",
                params![room_id, user2, nul, nul],
            )?;

            // 相互リンク更新
            tx.execute(
                "UPDATE room_members SET next = ?1, prev = ?2 WHERE room_id = ?3 AND user_id = ?4",
                params![user2, user2, room_id, user1],
            )?;
            tx.execute(
                "UPDATE room_members SET next = ?1, prev = ?2 WHERE room_id = ?3 AND user_id = ?4",
                params![user1, user1, room_id, user2],
            )?;
            Ok(())
        })?;

        Ok(())
    }

    #[tokio::test]
    async fn test_add_vote_state() -> Result<()> {
        let repo = setup_repo()?;
        init_test_data(&repo)?;

        repo.add_vote_state(1, 2, "apple").await?;

        let current_user: i64 = repo.db.query_row(
            "SELECT current_user_id FROM room_votes WHERE room_id = 1",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(current_user, 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_vote() -> Result<()> {
        let repo = setup_repo()?;
        init_test_data(&repo)?;
        repo.add_vote_state(1, 2, "apple").await?;

        repo.vote(1, 2, "good").await?;

        let state: String = repo.db.query_row(
            "SELECT state FROM member_votes WHERE room_id = 1 AND user_id = 2",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(state, "good");
        Ok(())
    }

    #[tokio::test]
    async fn test_next_user() -> Result<()> {
        let repo = setup_repo()?;
        init_test_data(&repo)?;
        repo.add_vote_state(1, 2, "apple").await?;

        repo.next_user(1).await?;

        let next_user: i64 = repo.db.query_row(
            "SELECT current_user_id FROM room_votes WHERE room_id = 1",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(next_user, 3);
        Ok(())
    }

    #[tokio::test]
    async fn test_delete_vote() -> Result<()> {
        let repo = setup_repo()?;
        init_test_data(&repo)?;
        repo.add_vote_state(1, 2, "apple").await?;
        repo.vote(1, 2, "good").await?;

        repo.delete_vote(1).await?;

        // room_votesにデータが消えているか確認
        let deleted: rusqlite::Result<i64> = repo.db.query_row(
            "SELECT current_user_id FROM room_votes WHERE room_id = 1",
            [],
            |row| row.get(0),
        );
        assert!(matches!(deleted, Err(rusqlite::Error::QueryReturnedNoRows)));

        // member_votesがnoneにリセットされているか確認
        let state_after: String = repo.db.query_row(
            "SELECT state FROM member_votes WHERE room_id = 1 AND user_id = 2",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(state_after, "none");
        Ok(())
    }

    #[tokio::test]
    async fn test_get_queue_cases() -> Result<()> {
        // --- 共通初期化 ---
        let repo = setup_repo()?;

        // ========== ① ルームが存在しない ==========
        {
            let result = repo.get_queue(999).await;
            // エラーが出ていないか（関数実行自体は成功）
            assert!(
                result.is_ok(),
                "roomが存在しない場合でもget_queueはErrを返すべきではない: {:?}",
                result
            );

            let queue = result.unwrap();
            assert!(
                queue.is_empty(),
                "存在しないルームは空配列を返すべき: got {:?}",
                queue
            );
        }

        // ========== ② 始点が設定されていない ==========
        {
            let room_id = 1;
            let result = repo.create_room(room_id).await;
            assert!(
                result.is_ok(),
                "create_roomでエラーが発生しました: {:?}",
                result
            );

            // まだqueueを設定していない
            let result = repo.get_queue(room_id).await;
            assert!(
                result.is_ok(),
                "終点未設定は空配列を返すべき: {:?}",
                result
            );
        }

        // ========== ③ 順序が破損している ==========
        {
            let room_id = 2;
            let result = repo.create_room(room_id).await;
            assert!(
                result.is_ok(),
                "create_roomでエラーが発生しました: {:?}",
                result
            );

            // 正常なqueueを設定
            let result = repo.set_queue(room_id, vec![1, 2]).await;
            assert!(
                result.is_ok(),
                "set_queueでエラーが発生しました: {:?}",
                result
            );

            // わざと破壊
            let sql_result = repo
                .db
                .execute("UPDATE room_members SET next = NULL WHERE user_id = 1", []);
            assert!(
                sql_result.is_ok(),
                "DB直接操作に失敗しました: {:?}",
                sql_result
            );

            let result = repo.get_queue(room_id).await;
            assert!(
                result.is_err(),
                "破損した順序はErrを返すべき: {:?}",
                result
            );

            if let Err(e) = result {
                assert!(
                    e.to_string().contains("BrokenChain"),
                    "BrokenChain エラーを期待したが、実際は {:?}",
                    e
                );
            }
        }

        // ========== ④ 正常な順序 ==========
        {
            let room_id = 3;
            let result = repo.create_room(room_id).await;
            assert!(
                result.is_ok(),
                "create_roomでエラーが発生しました: {:?}",
                result
            );

            let result = repo.set_queue(room_id, vec![10, 20, 30]).await;
            assert!(
                result.is_ok(),
                "set_queueでエラーが発生しました: {:?}",
                result
            );

            let result = repo.get_queue(room_id).await;
            assert!(
                result.is_ok(),
                "正常な順序取得でErrが返されました: {:?}",
                result
            );

            let queue = result.unwrap();
            assert_eq!(
                queue,
                vec![10, 20, 30],
                "正しい順序が取得できるべき: got {:?}",
                queue
            );
        }

        Ok(())
    }
}
