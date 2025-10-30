use rusqlite::{Connection, Params, Result};
use std::{fs, sync::Mutex};

/// SQLiteデータベースへの基本アクセスを提供する構造体
pub struct DataBase {
    conn: Mutex<Connection>,
}

impl DataBase {
    /// 新しいデータベース接続を作成します。
    /// pathに":memory:"を指定するとメモリ上に一時DBが作られます。
    pub fn new(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", &"ON")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 単純なSQL文を実行します（INSERT, UPDATE, DELETEなど）
    pub fn execute<P>(&self, sql: &str, params: P) -> Result<usize>
    where 
        P: Params
    {
        let conn = self.conn.lock().unwrap();
        conn.execute(sql, params)
    }

    /// SELECT文を実行し、1行だけ結果を取得します
    pub fn query_row<T, F>(&self, sql: &str, params: impl Params, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        let conn = self.conn.lock().unwrap();
        conn.query_row(sql, params, f)
    }

    /// SELECT文を実行し、複数行の結果をベクタとして返します
    pub fn query_map<T, F>(&self, sql: &str, params: impl Params, mut f: F) -> Result<Vec<T>>
    where
        F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params, |row| f(row))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// SQLiteの生コネクションを参照します（テストや低レベル操作用）
    pub fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    /// 一連の処理をトランザクションとして安全に実行します。
    /// 他スレッドからのアクセスはブロックされ、atomicに実行されます。
    pub fn exclusive_transaction<F, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> anyhow::Result<T>,
    {
        let mut conn = self.conn.lock().unwrap(); // ← 他スレッドをブロック
        let tx = conn.transaction()?; // ← トランザクション開始
        let result = f(&tx)?; // ← 処理本体
        tx.commit()?; // ← 一括コミット
        Ok(result)
    }

    /// スキーマ定義(schema.sqlなど)の読み込みに使用します。
    pub fn load_schema_from_file<P: AsRef<std::path::Path>>(&self, path: P) -> anyhow::Result<()> {
        let sql = fs::read_to_string(path)?;   // std::io::Error → anyhowが自動変換
        self.execute_batch(&sql)?;             // rusqlite::Error → anyhowが自動変換
        Ok(())
    }

    /// execute_batch（複数SQL文をまとめて実行）
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(sql)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::sync::Arc;
    use std::thread;

    fn setup_test_db() -> DataBase {
        DataBase::new(":memory:").expect("in-memory DB should open")
    }

    #[test]
    fn test_create_connection() {
        let db = setup_test_db();
        assert!(db.connection().is_autocommit());
    }

    #[test]
    fn test_create_table_and_insert() {
        let db = setup_test_db();
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", [])
            .unwrap();
        let inserted = db
            .execute("INSERT INTO users (name) VALUES (?1)", params!["Rikka"])
            .unwrap();
        assert_eq!(inserted, 1);
    }

    #[test]
    fn test_query_row() {
        let db = setup_test_db();
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", [])
            .unwrap();
        db.execute("INSERT INTO users (name) VALUES (?1)", params!["Rikka"])
            .unwrap();
        let name: String = db
            .query_row("SELECT name FROM users WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Rikka");
    }

    #[test]
    fn test_exclusive_transaction() {
        let db = setup_test_db();
        db.exclusive_transaction(|tx| {
            tx.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", [])?;
            tx.execute("INSERT INTO users (name) VALUES ('A')", [])?;
            Ok(())
        })
        .unwrap();

        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_multithread_access_is_safe() {
        let db = Arc::new(setup_test_db());
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", [])
            .unwrap();

        let db1 = db.clone();
        let t1 = thread::spawn(move || {
            db1.exclusive_transaction(|tx| {
                tx.execute("INSERT INTO users (name) VALUES ('ThreadA')", [])?;
                Ok(())
            })
            .unwrap();
        });

        let db2 = db.clone();
        let t2 = thread::spawn(move || {
            db2.exclusive_transaction(|tx| {
                tx.execute("INSERT INTO users (name) VALUES ('ThreadB')", [])?;
                Ok(())
            })
            .unwrap();
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let names: Vec<String> = db
            .query_map("SELECT name FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(names.len(), 2);
    }
}
