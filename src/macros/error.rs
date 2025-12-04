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
            Ok(val) => Ok::<_, RepoError>(val),
            Err(e) => match e {
                DatabaseError::Sqlite(sql_err) => match sql_err {
                    SqliteError::SqliteFailure(ref ffi_err, _) => {
                        match ffi_err.extended_code {
                            $(
                                code if code == ffi::$sqlite_code as i32 => Err($repo_variant),
                            )*
                            _ => Err(RepoError::Database(sql_err)),
                        }
                    }
                    _ => Err(RepoError::Database(sql_err)),
                },
                DatabaseError::Join(join_err) => {
                    Err(RepoError::Other(anyhow::anyhow!(join_err)))
                }
            }
        }
    }};
}
