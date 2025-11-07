#[macro_export]
macro_rules! db_to_repo {
    // $expr: Result<T, DatabaseError>
    // 第二引数: { SQLITE_CONSTRAINT_XXXX => RepoError::Something, ... }
    ($expr:expr, { $( $sqlite_code:ident => $repo_variant:expr ),* $(,)? }) => {{
        use rusqlite::Error as SqliteError;
        use rusqlite::ffi;
        use crate::database::db::DatabaseError;
        use crate::database::repository::RepoError;

        match $expr {
            Ok(val) => Ok(val),
            Err(e) => match e {
                DatabaseError::Sqlite(sql_err) => {
                    // rusqlite::Error::SqliteFailure を取り出す
                    if let SqliteError::SqliteFailure(ref ffi_err, _) = sql_err {
                        match ffi_err.extended_code {
                            $(
                                code if code == ffi::$sqlite_code as i32 => {
                                    return Err($repo_variant);
                                }
                            )*
                            // 該当しないSQLiteエラー
                            _ => return Err(RepoError::Database(sql_err)),
                        }
                    }
                    // SQLiteFailure以外のrusqliteエラー
                    Err(RepoError::Database(sql_err))
                }
                DatabaseError::Join(join_err) => {
                    Err(RepoError::Other(anyhow::anyhow!(join_err)))
                }
            }
        }
    }};
}
