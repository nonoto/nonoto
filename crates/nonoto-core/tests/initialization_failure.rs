//! SQLiteの初期化に失敗したときの診断と、その後に接続しないことを確かめる。
//!
//! SQLite全体のメモリの設定を変えるため、この1件だけを専用のテストのプロセスで動かす。
#![expect(
    unsafe_code,
    reason = "SQLiteのメモリの初期化を差し替えて失敗させるため、SQLiteの設定の関数を直接呼ぶ"
)]

use std::ffi::{c_int, c_void};
use std::mem::MaybeUninit;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use nonoto_core::{Error, Key, OpenStage, SqlCipherStore};
use rusqlite::ffi;

type MemoryInit = unsafe extern "C" fn(*mut c_void) -> c_int;

static ORIGINAL_INIT: OnceLock<Option<MemoryInit>> = OnceLock::new();
static INIT_CALLS: AtomicUsize = AtomicUsize::new(0);

/// 最初の1回だけ、SQLiteのメモリの初期化を失敗させる。以降は元の処理に戻す。
unsafe extern "C" fn fail_first_init(data: *mut c_void) -> c_int {
    if INIT_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
        return ffi::SQLITE_NOMEM;
    }
    match ORIGINAL_INIT.get().copied().flatten() {
        // SAFETY: SQLiteが元の初期化に渡す引数を、そのまま元の関数に渡す。
        Some(init) => unsafe { init(data) },
        None => ffi::SQLITE_OK,
    }
}

#[test]
fn 初期化に失敗したら最初のエラーを保ち接続しない() {
    let mut methods = MaybeUninit::<ffi::sqlite3_mem_methods>::uninit();
    // SAFETY: このプロセスには他のテストも接続も無い。GETMALLOCは指定した型の領域に設定を書き込む。
    let code = unsafe { ffi::sqlite3_config(ffi::SQLITE_CONFIG_GETMALLOC, methods.as_mut_ptr()) };
    assert_eq!(code, ffi::SQLITE_OK, "メモリの設定を取得できなかった");
    // SAFETY: GETMALLOCが成功したので、すべてのフィールドが初期化されている。
    let mut methods = unsafe { methods.assume_init() };
    ORIGINAL_INIT
        .set(methods.xInit)
        .expect("元のメモリの初期化を記録できなかった");
    methods.xInit = Some(fail_first_init);
    // SAFETY: 初期化の前に、同じ型の設定を渡す。SQLiteは設定をコピーし、コールバックはプロセスの終わりまで有効である。
    let code = unsafe { ffi::sqlite3_config(ffi::SQLITE_CONFIG_MALLOC, &raw const methods) };
    assert_eq!(code, ffi::SQLITE_OK, "メモリの設定を変更できなかった");

    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let path = dir.path().join("nonoto.db");
    let key = Key::generate().expect("鍵を作れなかった");
    for _ in 0..3 {
        let result = SqlCipherStore::open(&path, &key);
        assert!(
            matches!(
                result,
                Err(Error::OpenStore {
                    stage: OpenStage::Initialize,
                    extended_code: Some(ffi::SQLITE_NOMEM),
                    ..
                })
            ),
            "初期化の最初のエラーが残っていない: {:?}",
            result.err()
        );
        assert_eq!(
            INIT_CALLS.load(Ordering::SeqCst),
            1,
            "失敗した初期化を呼び直している"
        );
        assert!(!path.exists(), "初期化に失敗したのに接続している");
    }
}
