use rusqlite::{Connection, Params};
use std::sync::Arc;
use std::{fs::File, io::Write, path::Path};
use thiserror::Error;
use tokio::{sync::Mutex, task::JoinError};

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("Sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("非同期タスクエラー: {0}")]
    Join(#[from] JoinError),
}

pub type Result<T, E = DatabaseError> = std::result::Result<T, E>;

pub struct DataBase {
    conn: Arc<Mutex<Connection>>,
}

// Connection / Transaction 共通API定義
pub trait QueryExecutor {
    async fn execute(&self, sql: &str, params: impl Send + Params + 'static) -> Result<usize>;
    async fn query<T, F, P>(&self, sql: &str, params: P, f: F) -> Result<Vec<T>>
    where
        P: Send + Params + 'static,
        F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static;
}

impl QueryExecutor for DataBase {
    async fn execute(&self, sql: &str, params: impl Send + Params + 'static) -> Result<usize> {
        tokio::task::spawn_blocking({
            let conn = self.conn.clone();
            let sql = sql.to_string();
            move || {
                let conn = conn.blocking_lock();
                let count = conn.execute(&sql, params)?;
                Ok(count)
            }
        })
        .await?
    }

    async fn query<T, F, P>(&self, sql: &str, params: P, f: F) -> Result<Vec<T>>
    where
        P: Send + Params + 'static,
        F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        let sql = sql.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn.prepare(&sql)?;
            let mut f = f;
            let rows = stmt.query_map(params, |row| f(row))?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await?
    }
}

impl<'a> QueryExecutor for QueryTransaction<'a> {
    async fn execute(&self, sql: &str, params: impl Send + Params + 'static) -> Result<usize> {
        let sql = sql.to_string();
        let count = self.tx.execute(&sql, params)?;
        Ok(count)
    }

    async fn query<T, F, P>(&self, sql: &str, params: P, f: F) -> Result<Vec<T>>
    where
        P: Send + Params + 'static,
        F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let sql = sql.to_string();
        let mut f = f;

        let mut stmt = self.tx.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| f(row))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }
}

#[allow(dead_code)]
impl DataBase {
    /// 新しいデータベース接続を作成します。
    pub async fn new(path: &str, init_sql: Option<&str>) -> Result<Self> {
        let path_owned = path.to_owned();
        let init_sql_owned = init_sql.map(|s| s.to_owned());

        let conn = tokio::task::spawn_blocking(move || -> Result<Connection> {
            let db_exists = Path::new(&path_owned).exists();
            let conn = Connection::open(&path_owned)?;
            if !db_exists {
                if let Some(sql) = init_sql_owned.as_deref() {
                    conn.execute_batch(sql)?;
                }
            }
            conn.pragma_update(None, "foreign_keys", &"ON")?;
            Ok(conn)
        })
        .await??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// SELECT文を実行し、1行だけ結果を取得します
    #[deprecated(note = "query_row is deprecated, please use QueryExecutor::query instead")]
    pub async fn query_row<T, F, P>(&self, sql: &str, params: P, f: F) -> Result<T>
    where
        P: Send + Params + 'static,
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        let sql = sql.to_string();

        let result = tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.query_row(&sql, params, f)
        })
        .await??;

        Ok(result)
    }

    /// SELECT文を実行し、複数行の結果をベクタとして返します
    #[deprecated(note = "query_map is deprecated, please use QueryExecutor::query instead")]
    pub async fn query_map<T, F, P>(&self, sql: &str, params: P, mut f: F) -> Result<Vec<T>>
    where
        P: Send + Params + 'static,
        F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        let sql = sql.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params, |row| f(row))?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await?
    }

    /// 一連の処理をトランザクションとして安全に実行します。
    /// atomic に実行され、他スレッドからブロックされます。
    pub async fn exclusive_transaction<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<rusqlite::Error> + From<tokio::task::JoinError> + Send + 'static,
    {
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            let mut conn = conn.blocking_lock();
            let tx = conn.transaction()?;
            let result = f(&tx)?;
            tx.commit()?;
            Ok(result)
        })
        .await?
    }

    /// 複数SQL文をまとめて実行
    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        let conn = self.conn.clone();
        let sql = sql.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute_batch(&sql)?;
            Ok(())
        })
        .await?
    }

    /// schemaファイルを読み込む
    pub async fn load_schema(&self, sql: &str) -> Result<()> {
        self.execute_batch(&sql).await?;
        Ok(())
    }

    /// データベース情報ダンプ
    /// データベースをSQL形式でダンプし、指定したファイルに出力します。
    pub async fn dump_database<P: AsRef<Path>>(&self, path: P) -> Result<(), anyhow::Error> {
        let conn = self.conn.clone();
        let path = path.as_ref().to_path_buf();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut file = File::create(&path)?;

            // 1. sqlite_masterからスキーマ取得
            let mut stmt = conn.prepare(
                "SELECT type, name, tbl_name, sql FROM sqlite_master
                 WHERE type IN ('table', 'index', 'trigger', 'view')
                 ORDER BY type DESC, name",
            )?;

            let entries = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,         // type
                    row.get::<_, String>(1)?,         // name
                    row.get::<_, String>(2)?,         // tbl_name
                    row.get::<_, Option<String>>(3)?, // sql
                ))
            })?;

            // 2. 出力開始
            writeln!(file, "BEGIN TRANSACTION;")?;

            for entry in entries {
                let (ty, name, _tbl_name, sql) = entry?;
                if let Some(sql) = sql {
                    writeln!(file, "{};", sql)?;
                }

                // 3. テーブルデータをINSERT文で出力
                if ty == "table" && name != "sqlite_sequence" {
                    let mut stmt = conn.prepare(&format!("SELECT * FROM {}", name))?;
                    let column_count = stmt.column_count();
                    let mut rows = stmt.query([])?;

                    while let Some(row) = rows.next()? {
                        let mut values = Vec::with_capacity(column_count);
                        for i in 0..column_count {
                            let value: rusqlite::types::Value = row.get(i)?;
                            let val_str = match value {
                                rusqlite::types::Value::Null => "NULL".to_string(),
                                rusqlite::types::Value::Integer(v) => v.to_string(),
                                rusqlite::types::Value::Real(v) => v.to_string(),
                                rusqlite::types::Value::Text(t) => {
                                    format!("'{}'", t.replace('\'', "''"))
                                }
                                rusqlite::types::Value::Blob(_) => "'<BLOB>'".to_string(),
                            };
                            values.push(val_str);
                        }

                        writeln!(file, "INSERT INTO {} VALUES({});", name, values.join(", "))?;
                    }
                }
            }

            writeln!(file, "COMMIT;")?;
            Ok::<_, anyhow::Error>(())
        })
        .await?
    }
}

pub struct QueryTransaction<'conn> {
    tx: rusqlite::Transaction<'conn>,
}

impl<'conn> QueryTransaction<'conn> {
    pub fn new(tx: rusqlite::Transaction<'conn>) -> Self {
        Self { tx }
    }

    pub fn rollback(self) -> rusqlite::Result<()> {
        self.tx.rollback()
    }

    pub fn commit(self) -> rusqlite::Result<()> {
        self.tx.commit()
    }
}
