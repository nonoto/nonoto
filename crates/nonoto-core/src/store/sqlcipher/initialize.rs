//! SQLiteとSQLCipherのプロセスの初期化。
//!
//! 初期化の戻り値を受け取るためにSQLiteの関数を直接呼ぶ。unsafeのコードはこのモジュールにだけ置く。
#![expect(
    unsafe_code,
    reason = "rusqliteは初期化の失敗を読み捨てる経路があるため、sqlite3_initializeを直接呼んで戻り値を受け取る"
)]

use std::sync::OnceLock;

/// SQLiteとSQLCipherのプロセスの初期化を、1つのスレッドで済ませる。
///
/// SQLiteの `sqlite3_initialize` は、SQLCipherの初期化を終える前に初期化済みの印を立てる。
/// その間に別のスレッドが接続を開くと、SQLCipherが初期化されていない接続に鍵を渡すことになり、`PRAGMA key` が失敗する。
/// 鍵を渡す接続を開く前に、ここで初期化を最後まで済ませ、他のスレッドは済むまで待たせる。
/// 待たせられるのはこの関数を通るスレッドだけなので、プロセスの中の接続は、テストのものも含めてすべてこの関数の後に開く。
/// 失敗した場合も結果を保持する。SQLCipherだけが初期化されていない状態で、次の呼び出しが成功を返すことがあるためである。
pub(super) fn initialize() -> rusqlite::Result<()> {
    static RESULT: OnceLock<i32> = OnceLock::new();
    let code = *RESULT.get_or_init(|| {
        // SAFETY: sqlite3_initializeは引数を取らず、スレッドセーフで何度呼んでもよい (SQLiteのドキュメントのとおり)。
        // OnceLockが呼び出しを1回にまとめ、ほかのスレッドは完了まで待つ。
        unsafe { rusqlite::ffi::sqlite3_initialize() }
    });
    if code != rusqlite::ffi::SQLITE_OK {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            None,
        ));
    }
    Ok(())
}
