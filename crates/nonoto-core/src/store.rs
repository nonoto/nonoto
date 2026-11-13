mod sqlcipher;

use jiff::Timestamp;

use crate::error::Error;
use crate::page::{Page, PageId, PageSummary, PageUpdate};

pub use sqlcipher::SqlCipherStore;

/// ページを保存するストア。
///
/// 端末上の保存の方式 (データベース全体の暗号化・レコード単位の暗号化など) を、この裏に置く。
pub trait PageStore {
    /// ページを保存する。
    fn insert(&mut self, page: &Page) -> Result<(), Error>;

    /// ページを、更新した日時の新しい順に返す。
    fn list(&self) -> Result<Vec<PageSummary>, Error>;

    /// ページを返す。無ければ `None` を返す。
    fn get(&self, id: PageId) -> Result<Option<Page>, Error>;

    /// ページを更新し、更新したあとのページを返す。
    /// 無ければ [`Error::PageNotFound`] を返す。
    fn update(&mut self, id: PageId, update: PageUpdate, now: Timestamp) -> Result<Page, Error>;
}

#[cfg(test)]
pub(crate) mod tests {
    use tempfile::TempDir;

    use super::SqlCipherStore;
    use crate::key::Key;

    /// 一時ディレクトリにストアを開く。ディレクトリは、返した `TempDir` を捨てると消える。
    pub(crate) fn open_store() -> (TempDir, SqlCipherStore) {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let key = Key::generate().expect("鍵を作れなかった");
        let store = SqlCipherStore::open(&dir.path().join("nonoto.db"), &key)
            .expect("ストアを開けなかった");
        (dir, store)
    }
}
