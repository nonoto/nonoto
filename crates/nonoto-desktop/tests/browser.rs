use gpui_kit::test::{TestEventExt, TestWindowExt};
use gpui_kit::{
    AppContext, Bounds, Entity, Focusable, MouseButton, Point, TestAppContext, WindowBounds,
    WindowHandle, WindowOptions, base::Root, point, px, size,
};
use nonoto_core::{KeyStorage, NewPage, PageStore, Pages, SqlCipherStore};
use nonoto_desktop::PageBrowser;
use tempfile::TempDir;

mod common;
use common::FailingStore;

/// 一時ディレクトリにページを作り、作成した順と逆 (更新した日時の新しい順) で一覧に並ぶようにする。
fn pages_with(titles_and_bodies: &[(&str, &str)]) -> (TempDir, Pages<SqlCipherStore>) {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let mut pages = nonoto_core::open(dir.path(), KeyStorage::File).expect("ページを開けなかった");
    for (title, body) in titles_and_bodies {
        pages
            .create(NewPage {
                title: (*title).to_owned(),
                body: (*body).to_owned(),
            })
            .expect("ページを作成できなかった");
        // 一覧の並びを決めるため、更新日時 (ミリ秒) をずらす。
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    (dir, pages)
}

fn open_browser<S: PageStore + 'static>(
    cx: &mut TestAppContext,
    pages: Pages<S>,
) -> (WindowHandle<Root>, Entity<PageBrowser<S>>) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(1024.), px(720.)),
            })),
            ..Default::default()
        };
        let (window, browser) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| PageBrowser::new(pages, window, cx))
        })
        .expect("ウィンドウを開けなかった");
        (
            window.downcast::<Root>().expect("ウィンドウがRootではない"),
            browser,
        )
    })
}

#[gpui_kit::test]
fn 一覧を更新した日時の新しい順に並べ_先頭のページを表示する(
    cx: &mut TestAppContext,
) {
    let (_dir, pages) = pages_with(&[("古いページ", "古い本文"), ("新しいページ", "新しい本文")]);
    let (window, _) = open_browser(cx, pages);

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let items = window.find_all(("page", 0usize));
        assert_eq!(items.len(), 1, "一覧の先頭の項目が1つではない");
        assert_eq!(
            window.find(("page", 0usize)).label(),
            Some("新しいページ"),
            "一覧の先頭が新しいページではない"
        );
        assert_eq!(
            window.find(("page", 1usize)).label(),
            Some("古いページ"),
            "一覧の2番目が古いページではない"
        );
        assert_eq!(
            window.find(("page", 0usize)).selected(),
            Some(true),
            "先頭のページが選ばれていない"
        );
        assert_eq!(
            window.find("page-title").label(),
            Some("新しいページ"),
            "先頭のページのタイトルが表示されていない"
        );
    })
    .expect("ウィンドウを更新できなかった");
}

#[gpui_kit::test]
fn 一覧の項目を押すとそのページを表示する(cx: &mut TestAppContext) {
    let (_dir, pages) = pages_with(&[
        ("議事録", "# 東京都の会議\n\n本文"),
        ("はじめに", "ようこそ"),
    ]);
    let (window, browser) = open_browser(cx, pages);

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("page", 1usize), cx);
    })
    .expect("一覧の項目を押せなかった");
    cx.run_until_parked();

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("page-title").label(),
            Some("議事録"),
            "押したページのタイトルが表示されていない"
        );
        assert_eq!(
            window.find(("page", 1usize)).selected(),
            Some(true),
            "押したページが選ばれていない"
        );
        assert_eq!(
            window.find(("page", 0usize)).selected(),
            Some(false),
            "前に選んでいたページの選択が外れていない"
        );
    })
    .expect("ウィンドウを更新できなかった");

    cx.update(|cx| {
        let selected = browser.read(cx).selected().expect("ページが選ばれていない");
        assert_eq!(
            selected.body, "# 東京都の会議\n\n本文",
            "選んだページの本文が読み込まれていない"
        );
    });
}

#[gpui_kit::test]
fn ページが無ければその旨を表示する(cx: &mut TestAppContext) {
    let (_dir, pages) = pages_with(&[]);
    let (window, _) = open_browser(cx, pages);

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);

        assert_eq!(
            window.find("empty").label(),
            Some("ページがありません"),
            "ページが無い旨が表示されていない"
        );
        assert!(
            window.try_find(("page", 0usize)).is_none(),
            "ページが無いのに一覧に項目がある"
        );
    })
    .expect("ウィンドウを更新できなかった");
}

#[gpui_kit::test]
fn 空のタイトルは無題のページとして表示する(cx: &mut TestAppContext) {
    let (_dir, pages) = pages_with(&[("", "本文だけのページ")]);
    let (window, _) = open_browser(cx, pages);

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);

        assert_eq!(
            window.find(("page", 0usize)).label(),
            Some("無題のページ"),
            "空のタイトルの代わりの文言が一覧に出ていない"
        );
        assert_eq!(
            window.find("page-title").label(),
            Some("無題のページ"),
            "空のタイトルの代わりの文言が見出しに出ていない"
        );
    })
    .expect("ウィンドウを更新できなかった");
}

fn press(window: WindowHandle<Root>, key: &str, cx: &mut TestAppContext) {
    cx.update_window(window.into(), |_, window, cx| window.press(key, cx))
        .expect("キーを送れなかった");
    cx.run_until_parked();
}

#[gpui_kit::test]
fn tabで一覧へ入り矢印キーで別のページを表示できる(cx: &mut TestAppContext) {
    let (_dir, pages) = pages_with(&[("議事録", "会議の本文"), ("はじめに", "ようこそ")]);
    let (window, browser) = open_browser(cx, pages);
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let body = window.find("page-title").bounds().bottom_left() + point(px(8.), px(20.));
        window.pointer_down(body, MouseButton::Left, cx);
        window.pointer_up(body, MouseButton::Left, cx);
        assert!(
            !browser.read(cx).focus_handle(cx).is_focused(window),
            "本文を押しても一覧にフォーカスが残った"
        );
    })
    .expect("初期の描画を完了できなかった");

    press(window, "tab", cx);
    cx.update_window(window.into(), |_, window, cx| {
        assert!(
            browser.read(cx).focus_handle(cx).is_focused(window),
            "Tabで一覧へ入れない"
        );
    })
    .expect("フォーカスを確認できなかった");
    press(window, "down", cx);
    press(window, "enter", cx);

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("page-title").label(),
            Some("議事録"),
            "下矢印で別のページを表示できない"
        );
        assert_eq!(
            window.find(("page", 1usize)).selected(),
            Some(true),
            "表示したページが選ばれていない"
        );
        assert_eq!(
            browser
                .read(cx)
                .selected()
                .expect("ページが選ばれていない")
                .body,
            "会議の本文",
            "本文が切り替わらない"
        );
    })
    .expect("選択結果を確認できなかった");

    press(window, "up", cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("page-title").label(),
            Some("はじめに"),
            "上矢印で前のページへ戻れない"
        );
    })
    .expect("選択結果を確認できなかった");
}

#[gpui_kit::test]
fn 長い一覧でもキーボードで選んだ項目が画面内に表示される(
    cx: &mut TestAppContext,
) {
    let titles: Vec<_> = (0..60).map(|index| format!("ページ{index}")).collect();
    let refs: Vec<_> = titles
        .iter()
        .map(|title| (title.as_str(), "本文"))
        .collect();
    let (_dir, pages) = pages_with(&refs);
    let (window, _) = open_browser(cx, pages);
    cx.run_until_parked();

    for _ in 0..59 {
        press(window, "down", cx);
    }

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let last = window.find(("page", 59usize));
        assert!(last.visible(), "選んだ末尾の項目が画面内に表示されない");
        assert!(
            last.bounds().bottom() <= px(720.),
            "選んだ末尾の項目が画面からはみ出す"
        );
        assert_eq!(last.selected(), Some(true), "末尾の項目が選ばれていない");
        assert_eq!(
            window.find("page-title").label(),
            Some("ページ0"),
            "末尾のページの本文へ切り替わらない"
        );
    })
    .expect("末尾の項目を確認できなかった");
}

#[gpui_kit::test]
fn 本文を読めないページを選ぶとエラーだけを表示し_その先へ矢印キーで進める(
    cx: &mut TestAppContext,
) {
    let (_dir, pages) = pages_with(&[
        ("古いページ", "古い本文"),
        ("読めないページ", "読めない本文"),
        ("新しいページ", "新しい本文"),
    ]);
    let unreadable = pages.list().expect("一覧を読めなかった")[1].id;
    let mut store = FailingStore::new(pages);
    store.unreadable = Some(unreadable);
    let (window, browser) = open_browser(cx, Pages::new(store));
    cx.run_until_parked();

    press(window, "down", cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("page", 1usize)).selected(),
            Some(true),
            "読めないページの選択が保たれない"
        );
        assert!(
            window
                .find("error")
                .label()
                .is_some_and(|label| label.contains("本文を読めない")),
            "本文を読めない旨が表示されない"
        );
        assert!(
            window.try_find("page-title").is_none(),
            "読めないページを選んだのに前のページが表示されている"
        );
        assert!(
            window.try_find("empty").is_none(),
            "本文を読めないだけなのにページが無いと表示された"
        );
        assert!(
            browser.read(cx).selected().is_none(),
            "読めないページを選んだのに前のページが残っている"
        );
    })
    .expect("読めないページの表示を確認できなかった");

    press(window, "down", cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("page-title").label(),
            Some("古いページ"),
            "読めないページの先へ進めない"
        );
        assert_eq!(
            window.find(("page", 2usize)).selected(),
            Some(true),
            "読めないページの先のページが選ばれていない"
        );
        assert!(
            window.try_find("error").is_none(),
            "読めたページを表示してもエラーが残っている"
        );
    })
    .expect("読めないページの先の表示を確認できなかった");
}

#[gpui_kit::test]
fn 一覧を読めないときはエラーだけを表示する(cx: &mut TestAppContext) {
    let (_dir, pages) = pages_with(&[("ページ", "本文")]);
    let mut store = FailingStore::new(pages);
    store.fails_list = true;
    let (window, _) = open_browser(cx, Pages::new(store));

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .find("error")
                .label()
                .is_some_and(|label| label.contains("一覧を読めない")),
            "一覧を読めない旨が表示されない"
        );
        assert!(
            window.try_find("empty").is_none(),
            "一覧を読めないのにページが無いと表示された"
        );
        assert!(
            window.try_find(("page", 0usize)).is_none(),
            "一覧を読めないのに一覧に項目がある"
        );
    })
    .expect("ウィンドウを更新できなかった");
}
