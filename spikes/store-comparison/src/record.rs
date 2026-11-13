//! 方式B: 素のSQLiteに、ページのタイトルと本文をレコード単位で暗号化して保存する。
//!
//! 起動時にすべてのレコードを復号してメモリに持ち、検索はメモリ上のインデックスで行う。
//! 暗号文の形式 (版・ノンス・暗号文) は、同期でサーバーへ送る形式と共有することを想定している。

use std::collections::HashMap;
use std::path::Path;

use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use jiff::Timestamp;
use nonoto_core::{Error, Key, Page, PageId, PageStore, PageSummary, PageUpdate};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::index::BigramIndex;

/// 暗号文の形式の版。
const FORMAT_VERSION: u8 = 1;

/// XChaCha20-Poly1305のノンスの長さ (バイト)。
const NONCE_LEN: usize = 24;

/// レコード単位で暗号化するストア。
pub struct RecordStore {
    conn: Connection,
    cipher: XChaCha20Poly1305,
    pages: HashMap<PageId, Page>,
    index: BigramIndex,
}

impl RecordStore {
    /// データベースを開き、すべてのレコードを復号してインデックスを作る。
    pub fn open(path: &Path, key: &Key) -> Result<Self, Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS records (
                 id TEXT PRIMARY KEY NOT NULL,
                 sealed BLOB NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );",
        )?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes())
            .expect("鍵の長さはXChaCha20-Poly1305の鍵の長さと同じ");
        let mut store = Self {
            conn,
            cipher,
            pages: HashMap::new(),
            index: BigramIndex::default(),
        };
        store.load()?;
        Ok(store)
    }

    fn load(&mut self) -> Result<(), Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, sealed, created_at, updated_at FROM records")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut pages = Vec::new();
        for row in rows {
            let (id, sealed, created_at, updated_at) = row?;
            let id: PageId = id
                .parse()
                .map_err(|_| Error::CorruptData(format!("ページのIDがUUIDではない: {id}")))?;
            let (title, body) = open_sealed(&self.cipher, id, &sealed)?;
            pages.push(Page {
                id,
                title,
                body,
                created_at: timestamp(created_at)?,
                updated_at: timestamp(updated_at)?,
            });
        }
        drop(stmt);
        for page in pages {
            self.remember(page);
        }
        Ok(())
    }

    /// タイトルか本文に `query` を含むページを、更新した日時の新しい順に返す。
    pub fn search(&self, query: &str) -> Vec<PageSummary> {
        let matches = |page: &&Page| page.title.contains(query) || page.body.contains(query);
        let mut found: Vec<&Page> = match self.index.candidates(query) {
            Some(ids) => ids
                .iter()
                .filter_map(|id| self.pages.get(id))
                .filter(matches)
                .collect(),
            None => self.pages.values().filter(matches).collect(),
        };
        newest_first(&mut found);
        found.into_iter().map(summary).collect()
    }

    fn remember(&mut self, page: Page) {
        if let Some(old) = self.pages.remove(&page.id) {
            self.index.remove(old.id, &[&old.title, &old.body]);
        }
        self.index.add(page.id, &[&page.title, &page.body]);
        self.pages.insert(page.id, page);
    }

    fn write(&mut self, page: &Page, behavior: TransactionBehavior) -> Result<(), Error> {
        let sealed = seal(&self.cipher, page);
        let tx = self.conn.transaction_with_behavior(behavior)?;
        tx.execute(
            "INSERT INTO records (id, sealed, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET sealed = excluded.sealed, updated_at = excluded.updated_at",
            params![
                page.id.to_string(),
                sealed,
                page.created_at.as_millisecond(),
                page.updated_at.as_millisecond(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}

impl PageStore for RecordStore {
    fn insert(&mut self, page: &Page) -> Result<(), Error> {
        self.write(page, TransactionBehavior::Deferred)?;
        self.remember(page.clone());
        Ok(())
    }

    fn list(&self) -> Result<Vec<PageSummary>, Error> {
        let mut pages: Vec<&Page> = self.pages.values().collect();
        newest_first(&mut pages);
        Ok(pages.into_iter().map(summary).collect())
    }

    fn get(&self, id: PageId) -> Result<Option<Page>, Error> {
        Ok(self.pages.get(&id).cloned())
    }

    fn update(&mut self, id: PageId, update: PageUpdate, now: Timestamp) -> Result<Page, Error> {
        // 比較のための実装なので、別の接続からの書き込みは考えず、メモリ上の値を正とする。
        let exists = self
            .conn
            .query_row(
                "SELECT 1 FROM records WHERE id = ?1",
                params![id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let mut page = match (exists, self.pages.get(&id)) {
            (true, Some(page)) => page.clone(),
            _ => return Err(Error::PageNotFound(id)),
        };
        if let Some(title) = update.title {
            page.title = title;
        }
        if let Some(body) = update.body {
            page.body = body;
        }
        page.updated_at = now;
        self.write(&page, TransactionBehavior::Immediate)?;
        self.remember(page.clone());
        Ok(page)
    }
}

/// タイトルと本文を暗号化する。形式は「版 (1バイト)・ノンス (24バイト)・暗号文」。
///
/// ページのIDを追加の認証データにし、暗号文を別のレコードへ付け替えられないようにする。
fn seal(cipher: &XChaCha20Poly1305, page: &Page) -> Vec<u8> {
    let title_len = u32::try_from(page.title.len()).expect("タイトルは4GiBより短い");
    let mut plaintext = Vec::with_capacity(4 + page.title.len() + page.body.len());
    plaintext.extend_from_slice(&title_len.to_le_bytes());
    plaintext.extend_from_slice(page.title.as_bytes());
    plaintext.extend_from_slice(page.body.as_bytes());
    let nonce = XNonce::generate();
    let id = page.id.to_string();
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &plaintext,
                aad: id.as_bytes(),
            },
        )
        .expect("メモリ上の暗号化は失敗しない");
    let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    sealed.push(FORMAT_VERSION);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    sealed
}

/// 暗号文を復号し、タイトルと本文を返す。認証に失敗したら、鍵が違うものとして扱う。
fn open_sealed(
    cipher: &XChaCha20Poly1305,
    id: PageId,
    sealed: &[u8],
) -> Result<(String, String), Error> {
    let corrupt = || Error::CorruptData(format!("ページの暗号文の形式が正しくない: {id}"));
    let (&version, rest) = sealed.split_first().ok_or_else(corrupt)?;
    if version != FORMAT_VERSION || rest.len() < NONCE_LEN {
        return Err(corrupt());
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce = XNonce::try_from(nonce).map_err(|_| corrupt())?;
    let aad = id.to_string();
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| Error::WrongKey)?;
    let (len, rest) = plaintext.split_at_checked(4).ok_or_else(corrupt)?;
    let title_len = u32::from_le_bytes(len.try_into().map_err(|_| corrupt())?) as usize;
    let (title, body) = rest.split_at_checked(title_len).ok_or_else(corrupt)?;
    let title = String::from_utf8(title.to_vec()).map_err(|_| corrupt())?;
    let body = String::from_utf8(body.to_vec()).map_err(|_| corrupt())?;
    Ok((title, body))
}

/// 方式Aの一覧と同じく、更新した日時の新しい順 (同じならIDの大きい順) に並べる。
fn newest_first(pages: &mut [&Page]) {
    pages.sort_by_cached_key(|page| std::cmp::Reverse((page.updated_at, page.id.to_string())));
}

fn summary(page: &Page) -> PageSummary {
    PageSummary {
        id: page.id,
        title: page.title.clone(),
        created_at: page.created_at,
        updated_at: page.updated_at,
    }
}

fn timestamp(millisecond: i64) -> Result<Timestamp, Error> {
    Timestamp::from_millisecond(millisecond)
        .map_err(|_| Error::CorruptData(format!("日時が範囲外: {millisecond}")))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn titles(summaries: Vec<PageSummary>) -> Vec<String> {
        summaries.into_iter().map(|summary| summary.title).collect()
    }

    #[test]
    fn 開き直すと復号したページを取得し検索できる() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("records.db");
        let key = Key::generate().expect("鍵を作れなかった");
        let saved = page("議事録", "東京都の会議について", 1_000);
        RecordStore::open(&path, &key)
            .expect("ストアを開けなかった")
            .insert(&saved)
            .expect("ページを保存できなかった");

        let reopened = RecordStore::open(&path, &key).expect("ストアを開き直せなかった");

        assert_eq!(
            reopened.get(saved.id).expect("ページを取得できなかった"),
            Some(saved),
            "開き直したあとのページが保存したページと異なる"
        );
        assert_eq!(
            titles(reopened.search("東京都")),
            ["議事録"],
            "3文字で検索できない"
        );
        assert_eq!(
            titles(reopened.search("会議")),
            ["議事録"],
            "2文字で検索できない"
        );
        assert_eq!(
            titles(reopened.search("都")),
            ["議事録"],
            "1文字で検索できない"
        );
        assert!(
            reopened.search("京都府").is_empty(),
            "含まない語で見つかった"
        );
    }

    #[test]
    fn 更新すると古い本文では見つからない() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let key = Key::generate().expect("鍵を作れなかった");
        let mut store =
            RecordStore::open(&dir.path().join("records.db"), &key).expect("ストアを開けなかった");
        let saved = page("メモ", "古い本文", 1_000);
        store.insert(&saved).expect("ページを保存できなかった");
        let now = Timestamp::from_millisecond(2_000).expect("日時を作れなかった");

        store
            .update(
                saved.id,
                PageUpdate {
                    title: None,
                    body: Some("新しい本文".to_owned()),
                },
                now,
            )
            .expect("ページを更新できなかった");

        assert!(store.search("古い").is_empty(), "古い本文で見つかった");
        assert_eq!(
            titles(store.search("新しい")),
            ["メモ"],
            "新しい本文で見つからない"
        );
    }

    #[test]
    fn 違う鍵では開けない() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("records.db");
        let key = Key::generate().expect("鍵を作れなかった");
        RecordStore::open(&path, &key)
            .expect("ストアを開けなかった")
            .insert(&page("タイトル", "本文", 1_000))
            .expect("ページを保存できなかった");
        let other = Key::generate().expect("鍵を作れなかった");

        let result = RecordStore::open(&path, &other);

        assert!(
            matches!(result, Err(Error::WrongKey)),
            "違う鍵でのエラーが鍵のエラーではない: {:?}",
            result.err()
        );
    }

    #[test]
    fn ファイルにタイトルと本文の平文が残らない() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("records.db");
        let key = Key::generate().expect("鍵を作れなかった");
        RecordStore::open(&path, &key)
            .expect("ストアを開けなかった")
            .insert(&page("平文のタイトル", "plaintext-body-marker", 1_000))
            .expect("ページを保存できなかった");

        let bytes = std::fs::read(&path).expect("データベースのファイルを読めなかった");

        for plaintext in ["平文のタイトル", "plaintext-body-marker"] {
            assert!(
                !bytes
                    .windows(plaintext.len())
                    .any(|window| window == plaintext.as_bytes()),
                "ファイルに平文が残っている: {plaintext}"
            );
        }
    }

    #[test]
    fn 暗号文を別のレコードへ付け替えると復号できない() {
        let key = Key::generate().expect("鍵を作れなかった");
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes()).expect("鍵の長さが合わない");
        let sealed = seal(&cipher, &page("タイトル", "本文", 1_000));

        let result = open_sealed(&cipher, PageId::new(), &sealed);

        assert!(
            matches!(result, Err(Error::WrongKey)),
            "別のIDで復号できてしまった: {result:?}"
        );
    }
}
