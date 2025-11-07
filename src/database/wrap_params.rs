// src/db_params.rs
use rusqlite::types::Value;

/// ---
/// u64 → i64 にビットを変えずに再解釈する関数
/// ---
pub fn u64_to_i64_bitwise(u: u64) -> i64 {
    i64::from_ne_bytes(u.to_ne_bytes())
}

/// ---
/// i64 → u64 にビットを変えずに再解釈する関数
/// ---
pub fn i64_to_u64_bitwise(i: i64) -> u64 {
    u64::from_ne_bytes(i.to_ne_bytes())
}

/// ---
/// 任意の型を rusqlite::Value に変換するためのトレイト
/// ---
pub trait IntoValue {
    fn into_value(self) -> Value;
}

/// ---
/// 各型の IntoValue 実装
/// ---

impl IntoValue for u64 {
    fn into_value(self) -> Value {
        Value::Integer(u64_to_i64_bitwise(self))
    }
}

impl IntoValue for i64 {
    fn into_value(self) -> Value {
        Value::Integer(self)
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::Text(self.to_string())
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::Text(self)
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Integer(if self { 1 } else { 0 })
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Value {
        Value::Real(self)
    }
}

/// ---
/// 汎用関数：IntoValue を呼び出す
/// ---
pub fn to_value<T: IntoValue>(x: T) -> Value {
    x.into_value()
}

/// ---
/// 独自マクロ wrap_params!
/// rusqlite::params! と同様の構文で、
/// rusqlite::execute() にそのまま渡せる型を返す。
/// u64のみi64に自動調整されます。
/// ---
#[macro_export]
macro_rules! wrap_params {
    ( $( $x:expr ),* $(,)? ) => {{
        use $crate::database::wrap_params::{to_value};
        use rusqlite::params_from_iter;

        let values = vec![
            $( to_value($x) ),*
        ];

        // 直接 rusqlite::execute に渡せる形に変換
        params_from_iter(values)
    }};
}