//! SQLCipherとsqlite3mcを、同じストアのコードで比べるための実装。
//!
//! 既定ではsqlite3mcに、機能 `sqlcipher` ではSQLCipherにリンクする。
//! ストアの形 (テーブル・トランザクション・鍵の渡し方) は `nonoto-core` の `SqlCipherStore` に揃える。

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use zeroize::Zeroizing;

/// リンクしている暗号化の実装の名前。
pub const IMPLEMENTATION: &str = if cfg!(feature = "sqlcipher") {
    "SQLCipher"
} else {
    "sqlite3mc"
};

/// 32バイトの鍵。
pub struct Key(Zeroizing<[u8; 32]>);

impl Key {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut())?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub struct Page {
    pub id: String,
    pub title: String,
    pub body: String,
    pub updated_at: i64,
}

/// データベース全体を暗号化したストア。
pub struct Store {
    conn: Connection,
}

impl Store {
    /// データベースを開いて鍵を渡し、鍵で読めることを確かめてから、無ければテーブルを作る。
    pub fn open(path: &Path, key: &Key) -> rusqlite::Result<Self> {
        let mut conn = Connection::open(path)?;
        apply_key(&conn, key)?;
        // どちらの実装も、鍵を渡した時点では検証せず、最初にページを読んだときに失敗する。
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS pages (
                 id TEXT PRIMARY KEY NOT NULL,
                 title TEXT NOT NULL,
                 body TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );",
        )?;
        tx.commit()?;
        Ok(Self { conn })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn insert(&mut self, page: &Page) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO pages (id, title, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![page.id, page.title, page.body, page.updated_at],
        )?;
        tx.commit()
    }

    pub fn get(&self, id: &str) -> rusqlite::Result<Option<Page>> {
        self.conn
            .query_row(
                "SELECT title, body, updated_at FROM pages WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Page {
                        id: id.to_owned(),
                        title: row.get(0)?,
                        body: row.get(1)?,
                        updated_at: row.get(2)?,
                    })
                },
            )
            .optional()
    }

    /// `nonoto-core` のストアと同じく、書き込み権を得てから今の値を読み、本文を差し替えて保存する。
    /// ページが無ければ `false` を返す。
    pub fn update_body(&mut self, id: &str, body: &str, updated_at: i64) -> rusqlite::Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let title: Option<String> = tx
            .query_row(
                "SELECT title FROM pages WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(title) = title else {
            return Ok(false);
        };
        tx.execute(
            "UPDATE pages SET title = ?2, body = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, title, body, updated_at],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

/// 鍵の導出 (PBKDF2) を省くため、鍵をそのまま16進数で渡す。
///
/// sqlite3mcの既定の暗号 (ChaCha20-Poly1305) も、SQLCipherと同じ `x'...'` の書き方で生の鍵を受け付ける。
fn apply_key(conn: &Connection, key: &Key) -> rusqlite::Result<()> {
    let hex: Zeroizing<String> = Zeroizing::new(
        key.as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    );
    let statement = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", hex.as_str()));
    conn.execute_batch(&statement)
}
