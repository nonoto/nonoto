//! 画面のテストで共通に使うもの。

use jiff::Timestamp;
use nonoto_core::{Error, Page, PageId, PageStore, PageSummary, PageUpdate, Pages, SqlCipherStore};

/// 読み込みや書き込みに失敗するストア。失敗したときの画面の表示と挙動を確かめる。
///
/// 一覧と本文の読み込みは設定に従って失敗し、ページの作成と更新は常に失敗する。
pub struct FailingStore {
    pages: Pages<SqlCipherStore>,
    /// 本文の読み込みに失敗するページ。
    pub unreadable: Option<PageId>,
    /// 一覧の読み込みに失敗するか。
    pub fails_list: bool,
}

impl FailingStore {
    /// 読み込みには失敗しないストアを作る。
    pub fn new(pages: Pages<SqlCipherStore>) -> Self {
        Self {
            pages,
            unreadable: None,
            fails_list: false,
        }
    }
}

impl PageStore for FailingStore {
    fn insert(&mut self, _: &Page) -> Result<(), Error> {
        Err(Error::Io(std::io::Error::other("ページを作れない")))
    }

    fn list(&self) -> Result<Vec<PageSummary>, Error> {
        if self.fails_list {
            return Err(Error::CorruptData("一覧を読めない".to_owned()));
        }
        self.pages.list()
    }

    fn get(&self, id: PageId) -> Result<Option<Page>, Error> {
        if self.unreadable == Some(id) {
            return Err(Error::CorruptData("本文を読めない".to_owned()));
        }
        match self.pages.get(id) {
            Ok(page) => Ok(Some(page)),
            Err(Error::PageNotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn update(&mut self, _: PageId, _: PageUpdate, _: Timestamp) -> Result<Page, Error> {
        Err(Error::Io(std::io::Error::other("ページを更新できない")))
    }
}
