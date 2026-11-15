//! sqlite3mcのamalgamationを `libsqlite3` としてビルドし、`rusqlite` からリンクさせる。
//!
//! `libsqlite3-sys` は、同梱のSQLiteを使わないときはリンクのときに `-lsqlite3` を渡すだけなので、
//! ここでビルドしたライブラリのディレクトリを検索のパスに足せば、それにリンクされる。
//! 機能 `sqlcipher` では、`libsqlite3-sys` が同梱のSQLCipherをビルドするため、何もしない。

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=SQLITE3MC_SOURCE_DIR");
    if std::env::var_os("CARGO_FEATURE_SQLCIPHER").is_some() {
        return;
    }
    let dir = PathBuf::from(
        std::env::var_os("SQLITE3MC_SOURCE_DIR")
            .expect("SQLITE3MC_SOURCE_DIR が無い。このクレートのディレクトリでcargoを実行する"),
    );
    let source = dir.join("sqlite3mc_amalgamation.c");
    assert!(
        source.is_file(),
        "sqlite3mcのソースが無い。scripts/fetch-sqlite3mc.sh を実行する: {}",
        source.display()
    );
    println!("cargo:rerun-if-changed={}", source.display());

    // コンパイルのオプションは、`libsqlite3-sys` が同梱のSQLiteをビルドするときと揃える。
    // 比べるのは暗号化の実装の違いであり、SQLiteの機能の違いではないため。
    let mut build = cc::Build::new();
    build
        .file(&source)
        .include(&dir)
        .warnings(false)
        .cargo_metadata(false);
    for define in [
        "SQLITE_CORE",
        "SQLITE_DEFAULT_FOREIGN_KEYS=1",
        "SQLITE_ENABLE_API_ARMOR",
        "SQLITE_ENABLE_COLUMN_METADATA",
        "SQLITE_ENABLE_DBSTAT_VTAB",
        "SQLITE_ENABLE_FTS3",
        "SQLITE_ENABLE_FTS3_PARENTHESIS",
        "SQLITE_ENABLE_FTS5",
        "SQLITE_ENABLE_JSON1",
        "SQLITE_ENABLE_LOAD_EXTENSION=1",
        "SQLITE_ENABLE_RTREE",
        "SQLITE_ENABLE_STAT4",
        "SQLITE_SOUNDEX",
        "SQLITE_THREADSAFE=1",
        "SQLITE_USE_URI",
        "HAVE_USLEEP=1",
        "HAVE_ISNAN",
        "_POSIX_THREAD_SAFE_FUNCTIONS",
        // 一時ファイルに平文を書かないよう、一時的なデータはメモリに置く。SQLCipherの同梱のビルドと同じ。
        "SQLITE_TEMP_STORE=2",
    ] {
        match define.split_once('=') {
            Some((name, value)) => build.define(name, value),
            None => build.define(define, None),
        };
    }
    build.compile("sqlite3");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR が無い");
    println!("cargo:rustc-link-search=native={out_dir}");
}
