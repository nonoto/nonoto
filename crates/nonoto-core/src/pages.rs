use std::sync::{Mutex, PoisonError};

use jiff::Timestamp;

use crate::clock::{Clock, SystemClock};
use crate::error::Error;
use crate::page::{NewPage, Page, PageId, PageSummary, PageUpdate};
use crate::store::PageStore;

/// ページの作成・一覧・取得・更新を行う。
///
/// 日時は持っている時計から決めてストアに渡す。
#[derive(Debug)]
pub struct Pages<S> {
    store: S,
    // ストアがSyncならPagesもSyncにするため、時計をMutexに入れる。
    clock: Mutex<Box<dyn Clock + Send>>,
}

impl<S: PageStore> Pages<S> {
    /// システムの時計で日時を決める。
    pub fn new(store: S) -> Self {
        Self::with_clock(store, SystemClock)
    }

    /// 渡した時計で日時を決める。
    pub fn with_clock(store: S, clock: impl Clock + Send + 'static) -> Self {
        Self {
            store,
            clock: Mutex::new(Box::new(clock)),
        }
    }

    /// ページを作成する。
    pub fn create(&mut self, new_page: NewPage) -> Result<Page, Error> {
        let now = self.now();
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

    /// キーワードを空白で区切り、すべての語をタイトルに含むページを、更新した日時の新しい順に `limit` 件まで返す。
    ///
    /// Wikiリンクの補完の候補に使う。
    pub fn search_by_title(&self, keyword: &str, limit: usize) -> Result<Vec<PageSummary>, Error> {
        self.store.search_by_title(keyword, limit)
    }

    /// タイトルが完全に一致するページを返す。複数あれば、更新した日時の新しいページを返す。
    ///
    /// Wikiリンクのリンク先を決めるのに使う。
    pub fn find_by_title(&self, title: &str) -> Result<Option<PageSummary>, Error> {
        self.store.find_by_title(title)
    }

    /// ページを取得する。
    pub fn get(&self, id: PageId) -> Result<Page, Error> {
        self.store.get(id)?.ok_or(Error::PageNotFound(id))
    }

    /// ページを更新する。
    pub fn update(&mut self, id: PageId, update: PageUpdate) -> Result<Page, Error> {
        let now = self.now();
        self.store.update(id, update, now)
    }

    /// 時計の今の日時を、ストアが保存する精度 (ミリ秒) に切り詰めて返す。
    ///
    /// 切り詰めないと、作成や更新で返したページと、あとで取得したページの日時が食い違う。
    fn now(&mut self) -> Timestamp {
        // 作成と更新はPagesを排他的に借りるため、ロックを取らずに時計を使える。
        let clock = self.clock.get_mut().unwrap_or_else(PoisonError::into_inner);
        let millisecond = clock.now().as_millisecond();
        Timestamp::from_millisecond(millisecond)
            .expect("ミリ秒に切り詰めた日時は、元の日時と同じく表せる範囲にある")
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::panic::{RefUnwindSafe, UnwindSafe};

    use super::*;
    use crate::store::SqlCipherStore;
    use crate::store::tests::open_store;

    /// 呼ぶたびに、決まった日時から1秒ずつ進んだ日時を返す時計。
    #[derive(Debug)]
    struct SteppingClock {
        next: Cell<Timestamp>,
    }

    impl SteppingClock {
        fn starting_at(start: Timestamp) -> Self {
            Self {
                next: Cell::new(start),
            }
        }
    }

    impl Clock for SteppingClock {
        fn now(&self) -> Timestamp {
            let now = self.next.get();
            self.next.set(
                now.checked_add(jiff::SignedDuration::from_secs(1))
                    .expect("1秒進めた日時が表せる範囲にある"),
            );
            now
        }
    }

    fn timestamp(text: &str) -> Timestamp {
        text.parse().expect("日時を読めなかった")
    }

    #[test]
    fn pagesはストアに応じた自動トレイトを保つ() {
        fn assert_auto_traits<T: Send + Sync + UnwindSafe + RefUnwindSafe>() {}
        fn assert_send<T: Send>() {}

        assert_auto_traits::<Pages<()>>();
        assert_send::<Pages<SqlCipherStore>>();
    }

    #[test]
    fn 作成と更新の日時を渡した時計からミリ秒に切り詰めて決める() {
        let (_dir, store) = open_store();
        let clock = SteppingClock::starting_at(timestamp("2026-01-02T03:04:05.678901Z"));
        let mut pages = Pages::with_clock(store, clock);

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
                    title: None,
                    body: Some("新しい本文".to_owned()),
                },
            )
            .expect("ページを更新できなかった");

        assert_eq!(
            created.created_at,
            timestamp("2026-01-02T03:04:05.678Z"),
            "作成日時が時計の日時をミリ秒に切り詰めたものではない"
        );
        assert_eq!(
            updated.updated_at,
            timestamp("2026-01-02T03:04:06.678Z"),
            "更新日時が時計の次の日時をミリ秒に切り詰めたものではない"
        );
        assert_eq!(
            pages.get(created.id).expect("ページを取得できなかった"),
            updated,
            "取得したページの日時が更新で返したページと異なる"
        );
    }

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
        let mut pages = Pages::with_clock(
            store,
            SteppingClock::starting_at(timestamp("2026-01-02T03:04:05Z")),
        );
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
            updated.updated_at > created.updated_at,
            "更新日時が作成したときより進んでいない"
        );
    }
}
