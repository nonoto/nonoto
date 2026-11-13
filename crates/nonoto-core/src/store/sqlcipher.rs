use std::fmt::Write as _;
use std::path::Path;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use zeroize::Zeroize;

use super::PageStore;
use crate::error::Error;
use crate::key::Key;
use crate::page::{Page, PageId, PageSummary, PageUpdate};

/// スキーマの版。`PRAGMA user_version` に書く。
const SCHEMA_VERSION: i64 = 1;

/// データベース全体をSQLCipherで暗号化するストア。
pub struct SqlCipherStore {
    conn: Connection,
}

impl SqlCipherStore {
    /// データベースを鍵で開く。ファイルが無ければ作る。
    pub fn open(path: &Path, key: &Key) -> Result<Self, Error> {
        let mut conn = Connection::open(path)?;
        // 鍵が違うときにSQLCipherが標準エラーへ書くログを止める。失敗はエラーの値で呼び出し元に返す。
        conn.execute_batch("PRAGMA cipher_log_level = NONE;")?;
        apply_key(&conn, key)?;
        verify_key(&conn)?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }
}

/// SQLCipherに鍵を渡す。
///
/// パスフレーズからの鍵の導出 (PBKDF2) を省くため、鍵をそのまま16進数で渡す。
fn apply_key(conn: &Connection, key: &Key) -> Result<(), Error> {
    let mut statement = String::with_capacity(16 + key.as_bytes().len() * 2);
    statement.push_str("PRAGMA key = \"x'");
    for byte in key.as_bytes() {
        // Stringへの書き込みは失敗しない。
        let _ = write!(statement, "{byte:02x}");
    }
    statement.push_str("'\";");
    let result = conn.execute_batch(&statement);
    statement.zeroize();
    result.map_err(Error::from)
}

/// 鍵でデータベースを読めるかを確かめる。
///
/// SQLCipherは鍵を渡した時点では検証せず、最初にページを読んだときに失敗するため、ここで読んでみる。
fn verify_key(conn: &Connection) -> Result<(), Error> {
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
        .map_err(|err| match err.sqlite_error_code() {
            Some(rusqlite::ErrorCode::NotADatabase) => Error::WrongKey,
            _ => Error::Database(err),
        })
}

fn migrate(conn: &mut Connection) -> Result<(), Error> {
    // 書き込み権を待った接続も、直前の初期化が保存した版で判断する。
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < SCHEMA_VERSION {
        tx.execute_batch(
            "CREATE TABLE pages (
                 id TEXT PRIMARY KEY NOT NULL,
                 title TEXT NOT NULL,
                 body TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );",
        )?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;
    Ok(())
}

impl PageStore for SqlCipherStore {
    fn insert(&mut self, page: &Page) -> Result<(), Error> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO pages (id, title, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                page.id.to_string(),
                page.title,
                page.body,
                page.created_at.as_millisecond(),
                page.updated_at.as_millisecond(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn list(&self) -> Result<Vec<PageSummary>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_at, updated_at FROM pages ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (id, title, created_at, updated_at) = row?;
            Ok(PageSummary {
                id: parse_id(&id)?,
                title,
                created_at: parse_timestamp(created_at)?,
                updated_at: parse_timestamp(updated_at)?,
            })
        })
        .collect()
    }

    fn get(&self, id: PageId) -> Result<Option<Page>, Error> {
        select_page(&self.conn, id)
    }

    fn update(&mut self, id: PageId, update: PageUpdate, now: Timestamp) -> Result<Page, Error> {
        // 書き込み権を得てから現在の値を読み、別の接続の部分更新も引き継ぐ。
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut page = select_page(&tx, id)?.ok_or(Error::PageNotFound(id))?;
        if let Some(title) = update.title {
            page.title = title;
        }
        if let Some(body) = update.body {
            page.body = body;
        }
        page.updated_at = now;
        tx.execute(
            "UPDATE pages SET title = ?2, body = ?3, updated_at = ?4 WHERE id = ?1",
            params![
                id.to_string(),
                page.title,
                page.body,
                page.updated_at.as_millisecond()
            ],
        )?;
        tx.commit()?;
        Ok(page)
    }
}

fn select_page(conn: &Connection, id: PageId) -> Result<Option<Page>, Error> {
    let row = conn
        .query_row(
            "SELECT title, body, created_at, updated_at FROM pages WHERE id = ?1",
            params![id.to_string()],
            read_page_row,
        )
        .optional()?;
    row.map(|(title, body, created_at, updated_at)| {
        Ok(Page {
            id,
            title,
            body,
            created_at: parse_timestamp(created_at)?,
            updated_at: parse_timestamp(updated_at)?,
        })
    })
    .transpose()
}

fn read_page_row(row: &Row<'_>) -> rusqlite::Result<(String, String, i64, i64)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn parse_id(id: &str) -> Result<PageId, Error> {
    id.parse()
        .map_err(|_| Error::CorruptData(format!("ページのIDがUUIDではない: {id}")))
}

fn parse_timestamp(millisecond: i64) -> Result<Timestamp, Error> {
    Timestamp::from_millisecond(millisecond)
        .map_err(|_| Error::CorruptData(format!("日時が範囲外: {millisecond}")))
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;
    use crate::store::tests::open_store;

    fn page(title: &str, body: &str, millisecond: i64) -> Page {
        let at = Timestamp::from_millisecond(millisecond).expect("日時を作れなかった");
        Page {
            id: PageId::new(),
            title: title.to_owned(),
            body: body.to_owned(),
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn 未初期化のデータベースを同時に開いてページを共有できる() {
        for _ in 0..5 {
            let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
            let path = dir.path().join("nonoto.db");
            let key = Key::generate().expect("鍵を作れなかった");
            let barrier = Barrier::new(16);

            let mut stores = thread::scope(|scope| {
                let handles: Vec<_> = (0..16)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            SqlCipherStore::open(&path, &key).expect("同時にストアを開けなかった")
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("ストアを開くスレッドが失敗した"))
                    .collect::<Vec<_>>()
            });

            let saved = page("タイトル", "本文", 1_000);
            stores[0].insert(&saved).expect("ページを保存できなかった");
            for store in &stores {
                assert_eq!(
                    store.get(saved.id).expect("ページを取得できなかった"),
                    Some(saved.clone()),
                    "同時に開いた接続から同じページを読めない"
                );
            }
        }
    }

    #[test]
    fn 別の接続から同時に部分更新しても両方の変更が残る() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("nonoto.db");
        let key = Key::generate().expect("鍵を作れなかった");
        let mut first = SqlCipherStore::open(&path, &key).expect("ストアを開けなかった");
        let mut second = SqlCipherStore::open(&path, &key).expect("ストアを開けなかった");

        for _ in 0..20 {
            let saved = page("タイトル", "本文", 1_000);
            first.insert(&saved).expect("ページを保存できなかった");
            let now = Timestamp::from_millisecond(2_000).expect("日時を作れなかった");
            let barrier = Barrier::new(2);

            thread::scope(|scope| {
                let title = scope.spawn(|| {
                    barrier.wait();
                    first.update(
                        saved.id,
                        PageUpdate {
                            title: Some("新しいタイトル".to_owned()),
                            body: None,
                        },
                        now,
                    )
                });
                let body = scope.spawn(|| {
                    barrier.wait();
                    second.update(
                        saved.id,
                        PageUpdate {
                            title: None,
                            body: Some("新しい本文".to_owned()),
                        },
                        now,
                    )
                });
                title
                    .join()
                    .expect("タイトルを更新するスレッドが失敗した")
                    .expect("同時にタイトルを更新できなかった");
                body.join()
                    .expect("本文を更新するスレッドが失敗した")
                    .expect("同時に本文を更新できなかった");
            });

            let want = Page {
                title: "新しいタイトル".to_owned(),
                body: "新しい本文".to_owned(),
                updated_at: now,
                ..saved
            };
            for store in [&first, &second] {
                assert_eq!(
                    store.get(want.id).expect("ページを取得できなかった"),
                    Some(want.clone()),
                    "同時に更新したタイトルと本文が両方残っていない"
                );
            }
        }
    }

    #[test]
    fn 保存したページを取得できる() {
        let (_dir, mut store) = open_store();
        let saved = page("タイトル", "# 見出し\n\n本文", 1_000);

        store.insert(&saved).expect("ページを保存できなかった");

        let got = store.get(saved.id).expect("ページを取得できなかった");
        assert_eq!(got, Some(saved), "取得したページが保存したページと異なる");
    }

    #[test]
    fn 無いページはnoneを返す() {
        let (_dir, store) = open_store();

        let got = store.get(PageId::new()).expect("ページを取得できなかった");

        assert_eq!(got, None, "無いページが返った");
    }

    #[test]
    fn 一覧は更新した日時の新しい順に並ぶ() {
        let (_dir, mut store) = open_store();
        let old = page("古いページ", "", 1_000);
        let new = page("新しいページ", "", 2_000);
        store.insert(&old).expect("ページを保存できなかった");
        store.insert(&new).expect("ページを保存できなかった");

        let titles: Vec<String> = store
            .list()
            .expect("一覧を取得できなかった")
            .into_iter()
            .map(|summary| summary.title)
            .collect();

        assert_eq!(
            titles,
            ["新しいページ", "古いページ"],
            "一覧の並びが期待と異なる"
        );
    }

    #[test]
    fn 更新した項目と更新日時だけが変わる() {
        let (_dir, mut store) = open_store();
        let saved = page("タイトル", "本文", 1_000);
        store.insert(&saved).expect("ページを保存できなかった");
        let now = Timestamp::from_millisecond(2_000).expect("日時を作れなかった");

        let updated = store
            .update(
                saved.id,
                PageUpdate {
                    title: None,
                    body: Some("新しい本文".to_owned()),
                },
                now,
            )
            .expect("ページを更新できなかった");

        let want = Page {
            body: "新しい本文".to_owned(),
            updated_at: now,
            ..saved
        };
        assert_eq!(updated, want, "更新したあとのページが期待と異なる");
        let got = store.get(want.id).expect("ページを取得できなかった");
        assert_eq!(got, Some(want), "保存されたページが更新した内容と異なる");
    }

    #[test]
    fn 無いページを更新するとページが見つからないエラーになる() {
        let (_dir, mut store) = open_store();
        let id = PageId::new();

        let err = store
            .update(id, PageUpdate::default(), Timestamp::now())
            .expect_err("無いページを更新できてしまった");

        assert!(
            matches!(err, Error::PageNotFound(got) if got == id),
            "エラーがページが見つからないエラーではない: {err:?}"
        );
    }

    #[test]
    fn 同じ鍵で開き直すと保存したページを読める() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("nonoto.db");
        let key = Key::generate().expect("鍵を作れなかった");
        let saved = page("タイトル", "本文", 1_000);
        SqlCipherStore::open(&path, &key)
            .expect("ストアを開けなかった")
            .insert(&saved)
            .expect("ページを保存できなかった");

        let reopened = SqlCipherStore::open(&path, &key).expect("ストアを開き直せなかった");

        let got = reopened.get(saved.id).expect("ページを取得できなかった");
        assert_eq!(got, Some(saved), "開き直したあとにページを読めない");
    }

    #[test]
    fn 違う鍵では開けない() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("nonoto.db");
        let key = Key::generate().expect("鍵を作れなかった");
        SqlCipherStore::open(&path, &key).expect("ストアを開けなかった");
        let other = Key::generate().expect("鍵を作れなかった");

        let result = SqlCipherStore::open(&path, &other);

        assert!(
            matches!(result, Err(Error::WrongKey)),
            "違う鍵でのエラーが鍵のエラーではない: {:?}",
            result.err()
        );
    }

    #[test]
    fn ファイルにタイトルと本文の平文が残らない() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("nonoto.db");
        let key = Key::generate().expect("鍵を作れなかった");
        let mut store = SqlCipherStore::open(&path, &key).expect("ストアを開けなかった");
        store
            .insert(&page("平文のタイトル", "plaintext-body-marker", 1_000))
            .expect("ページを保存できなかった");
        drop(store);

        let mut bytes = std::fs::read(&path).expect("データベースのファイルを読めなかった");
        for suffix in ["-wal", "-journal"] {
            if let Ok(extra) = std::fs::read(dir.path().join(format!("nonoto.db{suffix}"))) {
                bytes.extend(extra);
            }
        }

        for plaintext in ["平文のタイトル", "plaintext-body-marker"] {
            assert!(
                !contains(&bytes, plaintext.as_bytes()),
                "ファイルに平文が残っている: {plaintext}"
            );
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}
