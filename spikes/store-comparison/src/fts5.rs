//! SQLiteの接続に、FTS5のトークナイザー ([`Tokenizer`]) をCのAPIで登録する。
//!
//! `rusqlite` にはトークナイザーを登録するAPIが無いため、`SELECT fts5(?1)` で `fts5_api` のポインタを受け取り、
//! `xCreateTokenizer` を呼ぶ。このクレートの `unsafe` は、このモジュールだけに置く。

#![expect(
    unsafe_code,
    reason = "rusqliteにはFTS5のトークナイザーを登録するAPIが無いため、SQLiteのCのAPIを直接呼ぶ"
)]

use std::ffi::{CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use rusqlite::types::{ToSql, ToSqlOutput};
use rusqlite::{Connection, ffi};

use crate::tokenizer::{Purpose, Token, Tokenizer};

/// 登録するトークナイザー。FTS5に渡すユーザーデータとして、要素のアドレスを使う。
static TOKENIZERS: [Tokenizer; 2] = [Tokenizer::Bigram, Tokenizer::Unigram];

/// 接続にトークナイザーを登録する。FTS5のテーブルに触る前に、接続ごとに呼ぶ。
///
/// `fts5_api` を受け取る `SELECT` はスキーマを読むため、SQLCipherの接続では鍵を渡したあとに呼ぶ。
pub fn register(conn: &Connection) -> rusqlite::Result<()> {
    let api = fts5_api(conn)?;
    for tokenizer in &TOKENIZERS {
        let name = CString::new(tokenizer.name()).expect("トークナイザーの名前にNULが含まれる");
        let mut methods = ffi::fts5_tokenizer {
            xCreate: Some(create),
            xDelete: Some(delete),
            xTokenize: Some(tokenize),
        };
        // SAFETY: `api` は `fts5_api` が返した、この接続が開いている間有効なポインタである。
        // `xCreateTokenizer` は `methods` の中身をコピーして持つため、呼び出しのあとに `methods` が無くなってもよい。
        // ユーザーデータは `'static` の要素を指し、FTS5がそれを書き換えることはない。
        let rc = unsafe {
            let create_tokenizer = (*api).xCreateTokenizer.ok_or_else(|| {
                rusqlite::Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_ERROR), None)
            })?;
            create_tokenizer(
                api,
                name.as_ptr(),
                ptr::from_ref(tokenizer).cast_mut().cast(),
                &raw mut methods,
                None,
            )
        };
        check(rc)?;
    }
    Ok(())
}

/// `SELECT fts5(?1)` に `fts5_api` のポインタの置き場を渡し、接続の `fts5_api` を受け取る。
fn fts5_api(conn: &Connection) -> rusqlite::Result<*mut ffi::fts5_api> {
    let mut api: *mut ffi::fts5_api = ptr::null_mut();
    conn.query_row("SELECT fts5(?1)", [ApiSlot(&raw mut api)], |_| Ok(()))?;
    if api.is_null() {
        return Err(rusqlite::Error::SqliteFailure(
            ffi::Error::new(ffi::SQLITE_ERROR),
            Some("FTS5のAPIを受け取れなかった".to_owned()),
        ));
    }
    Ok(api)
}

/// `fts5_api` のポインタを書き込んでもらう置き場。SQLiteのポインタ渡しの型名 `fts5_api_ptr` で束縛する。
struct ApiSlot(*mut *mut ffi::fts5_api);

impl ToSql for ApiSlot {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Pointer((
            self.0.cast_const().cast(),
            c"fts5_api_ptr",
            None,
        )))
    }
}

fn check(rc: c_int) -> rusqlite::Result<()> {
    if rc == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), None))
    }
}

/// FTS5のテーブルを開くときに呼ばれる。トークナイザーには引数を取らせない。
unsafe extern "C" fn create(
    user_data: *mut c_void,
    _: *mut *const c_char,
    argument_count: c_int,
    out: *mut *mut ffi::Fts5Tokenizer,
) -> c_int {
    if argument_count != 0 {
        return ffi::SQLITE_ERROR;
    }
    // SAFETY: `out` はFTS5が用意した書き込み先である。
    // トークナイザーの状態を持たないため、登録したときのユーザーデータ (`TOKENIZERS` の要素) をそのまま返す。
    unsafe { *out = user_data.cast() };
    ffi::SQLITE_OK
}

/// `create` で返したのは `'static` の要素なので、解放するものは無い。
unsafe extern "C" fn delete(_: *mut ffi::Fts5Tokenizer) {}

/// テキストをトークンに分け、トークンごとに `emit` を呼ぶ。
unsafe extern "C" fn tokenize(
    tokenizer: *mut ffi::Fts5Tokenizer,
    context: *mut c_void,
    flags: c_int,
    text: *const c_char,
    length: c_int,
    emit: Option<
        unsafe extern "C" fn(*mut c_void, c_int, *const c_char, c_int, c_int, c_int) -> c_int,
    >,
) -> c_int {
    let Some(emit) = emit else {
        return ffi::SQLITE_ERROR;
    };
    let Ok(length) = usize::try_from(length) else {
        return ffi::SQLITE_ERROR;
    };
    if length == 0 {
        return ffi::SQLITE_OK;
    }
    // SAFETY: `tokenizer` は `create` が返した `TOKENIZERS` の要素を指す。
    let tokenizer = unsafe { *tokenizer.cast::<Tokenizer>() };
    // SAFETY: FTS5は、この呼び出しの間有効な `length` バイトのテキストを渡す。
    let bytes = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return ffi::SQLITE_ERROR;
    };
    let purpose = if flags & ffi::FTS5_TOKENIZE_QUERY != 0 {
        Purpose::Query
    } else {
        Purpose::Document
    };
    let emit_token = |token: Token<'_>| {
        // トークンと位置はテキストの中に収まり、テキストの長さが `c_int` に収まるため、変換は失敗しない。
        let to_int = |value: usize| c_int::try_from(value).map_err(|_| ffi::SQLITE_ERROR);
        // SAFETY: `context` と `emit` はFTS5が渡したものをそのまま使い、
        // トークンはこの呼び出しの間有効なテキストの一部を指す。
        let rc = unsafe {
            emit(
                context,
                0,
                token.text.as_ptr().cast(),
                to_int(token.text.len())?,
                to_int(token.start)?,
                to_int(token.end)?,
            )
        };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(rc)
        }
    };
    // CのAPIの呼び出し元へパニックを伝えないよう、エラーの値に変える。
    match catch_unwind(AssertUnwindSafe(|| {
        tokenizer.tokenize(text, purpose, emit_token)
    })) {
        Ok(Ok(())) => ffi::SQLITE_OK,
        Ok(Err(rc)) => rc,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 登録したトークナイザーで全文検索のテーブルを作れる() {
        let conn = Connection::open_in_memory().expect("データベースを開けなかった");
        register(&conn).expect("トークナイザーを登録できなかった");

        for tokenizer in TOKENIZERS {
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE t_{name} USING fts5(body, tokenize='{name}');
                 INSERT INTO t_{name}(body) VALUES ('東京都の会議');",
                name = tokenizer.name()
            ))
            .unwrap_or_else(|error| panic!("{tokenizer:?} のテーブルを作れなかった: {error}"));
            let count: i64 = conn
                .query_row(
                    &format!(
                        "SELECT count(*) FROM t_{} WHERE t_{0} MATCH '\"京都\"'",
                        tokenizer.name()
                    ),
                    [],
                    |row| row.get(0),
                )
                .expect("全文検索できなかった");
            assert_eq!(count, 1, "{tokenizer:?} で語を引けなかった");
        }
    }

    #[test]
    fn 引数を付けたトークナイザーではテーブルを作れない() {
        let conn = Connection::open_in_memory().expect("データベースを開けなかった");
        register(&conn).expect("トークナイザーを登録できなかった");

        let result =
            conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(body, tokenize='bigram x');");

        assert!(result.is_err(), "引数を付けてもテーブルを作れた");
    }

    #[test]
    fn 登録していない接続ではテーブルを作れない() {
        let conn = Connection::open_in_memory().expect("データベースを開けなかった");

        let result =
            conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(body, tokenize='bigram');");

        assert!(
            result.is_err(),
            "登録していないトークナイザーでテーブルを作れた"
        );
    }
}
