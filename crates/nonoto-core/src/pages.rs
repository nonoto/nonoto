use jiff::Timestamp;

use crate::error::Error;
use crate::page::{NewPage, Page, PageId, PageSummary, PageUpdate};
use crate::store::PageStore;

/// ページの作成・一覧・取得・更新を行う。
///
/// 日時はここで決めてストアに渡す。
pub struct Pages<S> {
    store: S,
}

impl<S: PageStore> Pages<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// ページを作成する。
    pub fn create(&mut self, new_page: NewPage) -> Result<Page, Error> {
        let now = now();
        let page = Page {
            id: PageId::new(),
            title: new_page.title,
            body: new_page.body,
            created_at: now,
            updated_at: now,
        };
        self.store.insert(&page)?;
        Ok(page)
    }

    /// ページを、更新した日時の新しい順に返す。
    pub fn list(&self) -> Result<Vec<PageSummary>, Error> {
        self.store.list()
    }

    /// ページを取得する。
    pub fn get(&self, id: PageId) -> Result<Page, Error> {
        self.store.get(id)?.ok_or(Error::PageNotFound(id))
    }

    /// ページを更新する。
    pub fn update(&mut self, id: PageId, update: PageUpdate) -> Result<Page, Error> {
        self.store.update(id, update, now())
    }
}

/// 今の日時を、ストアが保存する精度 (ミリ秒) に切り詰めて返す。
///
/// 切り詰めないと、作成や更新で返したページと、あとで取得したページの日時が食い違う。
fn now() -> Timestamp {
    let millisecond = Timestamp::now().as_millisecond();
    Timestamp::from_millisecond(millisecond).expect("今の日時はミリ秒で表せる範囲にある")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::open_store;

    #[test]
    fn 作成したページを取得できる() {
        let (_dir, store) = open_store();
        let mut pages = Pages::new(store);

        let created = pages
            .create(NewPage {
                title: "はじめに".to_owned(),
                body: "# 見出し\n\n本文".to_owned(),
            })
            .expect("ページを作成できなかった");

        assert_eq!(
            created.created_at, created.updated_at,
            "作成した直後の作成日時と更新日時が異なる"
        );
        let got = pages.get(created.id).expect("ページを取得できなかった");
        assert_eq!(got, created, "取得したページが作成したページと異なる");
    }

    #[test]
    fn 無いページを取得するとページが見つからないエラーになる() {
        let (_dir, store) = open_store();
        let pages = Pages::new(store);
        let id = PageId::new();

        let err = pages.get(id).expect_err("無いページを取得できてしまった");

        assert!(
            matches!(err, Error::PageNotFound(got) if got == id),
            "エラーがページが見つからないエラーではない: {err:?}"
        );
    }

    #[test]
    fn 更新すると更新日時だけが進む() {
        let (_dir, store) = open_store();
        let mut pages = Pages::new(store);
        let created = pages
            .create(NewPage {
                title: "タイトル".to_owned(),
                body: "本文".to_owned(),
            })
            .expect("ページを作成できなかった");

        let updated = pages
            .update(
                created.id,
                PageUpdate {
                    title: Some("新しいタイトル".to_owned()),
                    body: None,
                },
            )
            .expect("ページを更新できなかった");

        assert_eq!(
            updated.title, "新しいタイトル",
            "タイトルが更新されていない"
        );
        assert_eq!(updated.body, "本文", "指定しなかった本文が変わった");
        assert_eq!(updated.created_at, created.created_at, "作成日時が変わった");
        assert!(
            updated.updated_at >= created.updated_at,
            "更新日時が作成したときより前になった"
        );
    }
}
