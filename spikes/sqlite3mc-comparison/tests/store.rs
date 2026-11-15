use std::path::Path;

use sqlite3mc_comparison::{IMPLEMENTATION, Key, Page, Store};

fn page(id: &str, title: &str, body: &str) -> Page {
    Page {
        id: id.to_owned(),
        title: title.to_owned(),
        body: body.to_owned(),
        updated_at: 1_700_000_000_000,
    }
}

fn open(path: &Path, key: &Key) -> Store {
    Store::open(path, key).expect("ストアを開けなかった")
}

#[test]
fn リンクしている実装が名前と一致する() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let key = Key::generate().expect("鍵を作れなかった");
    let store = open(&dir.path().join("a.db"), &key);

    // sqlite3mcは sqlite3mc_version() を、SQLCipherは PRAGMA cipher_version を持つ。
    let sql = if IMPLEMENTATION == "sqlite3mc" {
        "SELECT sqlite3mc_version()"
    } else {
        "PRAGMA cipher_version"
    };
    let version: String = store
        .connection()
        .query_row(sql, [], |row| row.get(0))
        .expect("暗号化の実装の版を取得できなかった");

    assert!(!version.is_empty(), "暗号化の実装の版が空");
}

#[test]
fn 開き直すと保存したページを取得できる() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let key = Key::generate().expect("鍵を作れなかった");
    let path = dir.path().join("a.db");
    open(&path, &key)
        .insert(&page("p1", "会議の議事録", "東京都の会議"))
        .expect("ページを保存できなかった");

    let mut store = open(&path, &key);
    let saved = store
        .get("p1")
        .expect("ページを取得できなかった")
        .expect("保存したページが無い");
    assert_eq!(
        (saved.title.as_str(), saved.body.as_str()),
        ("会議の議事録", "東京都の会議"),
        "保存した内容と違う"
    );

    assert!(
        store
            .update_body("p1", "更新した本文", 1_700_000_000_001)
            .expect("ページを更新できなかった"),
        "ページを更新できなかった"
    );
    assert_eq!(
        open(&path, &key)
            .get("p1")
            .expect("ページを取得できなかった")
            .expect("更新したページが無い")
            .body,
        "更新した本文",
        "更新した本文が保存されていない"
    );
    assert!(
        !store
            .update_body("missing", "本文", 1)
            .expect("無いページの更新でエラーになった"),
        "無いページを更新したことになった"
    );
}

#[test]
fn 違う鍵では開けない() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let path = dir.path().join("a.db");
    open(&path, &Key::generate().expect("鍵を作れなかった"))
        .insert(&page("p1", "タイトル", "本文"))
        .expect("ページを保存できなかった");

    let result = Store::open(&path, &Key::generate().expect("鍵を作れなかった"));

    assert!(result.is_err(), "違う鍵で開けた");
}

#[test]
fn ファイルにタイトルと本文の平文が残らない() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let key = Key::generate().expect("鍵を作れなかった");
    let path = dir.path().join("a.db");
    let mut store = open(&path, &key);
    store
        .insert(&page(
            "p1",
            "平文で残ってはいけないタイトル",
            "平文で残ってはいけない本文",
        ))
        .expect("ページを保存できなかった");
    drop(store);

    let mut bytes = Vec::new();
    for entry in std::fs::read_dir(dir.path()).expect("ディレクトリを読めなかった") {
        bytes.extend(
            std::fs::read(entry.expect("ファイルを読めなかった").path())
                .expect("ファイルを読めなかった"),
        );
    }
    for plaintext in [
        "平文で残ってはいけないタイトル",
        "平文で残ってはいけない本文",
    ] {
        assert!(
            !bytes
                .windows(plaintext.len())
                .any(|window| window == plaintext.as_bytes()),
            "ファイルに平文が残った: {plaintext}"
        );
    }
}

#[test]
fn 全文検索のテーブルを作れる() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let key = Key::generate().expect("鍵を作れなかった");
    let mut store = open(&dir.path().join("a.db"), &key);
    store
        .insert(&page("p1", "会議の議事録", "東京都の会議"))
        .expect("ページを保存できなかった");
    let conn = store.connection();

    conn.execute_batch(
        "CREATE VIRTUAL TABLE pages_fts USING fts5(title, body, content='pages', tokenize='trigram');
         INSERT INTO pages_fts(pages_fts) VALUES ('rebuild');",
    )
    .expect("全文検索のテーブルを作れなかった");
    let found: i64 = conn
        .query_row(
            "SELECT count(*) FROM pages_fts WHERE pages_fts MATCH '\"東京都\"'",
            [],
            |row| row.get(0),
        )
        .expect("全文検索できなかった");

    assert_eq!(found, 1, "全文検索で見つからない");
}
