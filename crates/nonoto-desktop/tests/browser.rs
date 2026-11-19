use gpui_kit::test::{TestEventExt, TestWindowExt};
use gpui_kit::{
    AppContext, Bounds, Entity, Focusable, MouseButton, Point, TestAppContext, WindowBounds,
    WindowHandle, WindowOptions, base::Root, point, px, size,
};
use nonoto_core::{KeyStorage, NewPage, PageStore, Pages, SqlCipherStore};
use nonoto_desktop::PageBrowser;
use tempfile::TempDir;

mod common;
use common::{FailingStore, SteppingClock};

/// 一時ディレクトリにページを作り、作成した順と逆 (更新した日時の新しい順) で一覧に並ぶようにする。
fn pages_with(titles_and_bodies: &[(&str, &str)]) -> (TempDir, Pages<SqlCipherStore>) {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let mut pages =
        nonoto_core::open_with_clock(dir.path(), KeyStorage::File, SteppingClock::new())
            .expect("ページを開けなかった");
    for (title, body) in titles_and_bodies {
        pages
            .create(NewPage {
                title: (*title).to_owned(),
                body: (*body).to_owned(),
            })
            .expect("ページを作成できなかった");
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

/// 表示している本文の先頭 (最初の行の最初の文字の上) を、左のボタンで押す。
fn click_body_start(window: WindowHandle<Root>, cx: &mut TestAppContext) {
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let start = window.find("page-body").bounds().origin + point(px(6.), px(8.));
        window.pointer_down(start, MouseButton::Left, cx);
        window.pointer_up(start, MouseButton::Left, cx);
    })
    .expect("本文を押せなかった");
    cx.run_until_parked();
}

fn shown_title(window: WindowHandle<Root>, cx: &mut TestAppContext) -> Option<String> {
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.find("page-title").label().map(str::to_owned)
    })
    .expect("ウィンドウを更新できなかった")
}

#[gpui_kit::test]
fn 表示のwikiリンクを押すと同じタイトルのページへ移る(
    cx: &mut TestAppContext,
) {
    let (_dir, pages) = pages_with(&[
        ("はじめに", "古いページ"),
        ("はじめに", "新しいページ"),
        ("目次", "[[はじめに]]を読む"),
    ]);
    let (window, browser) = open_browser(cx, pages);
    cx.run_until_parked();
    assert_eq!(shown_title(window, cx).as_deref(), Some("目次"));

    click_body_start(window, cx);
    assert_eq!(
        shown_title(window, cx).as_deref(),
        Some("はじめに"),
        "Wikiリンクを押してもリンク先のページへ移らない"
    );
    cx.update(|cx| {
        assert_eq!(
            browser.read(cx).selected().map(|page| page.body.as_str()),
            Some("新しいページ"),
            "同じタイトルのページのうち、新しいページへ移っていない"
        );
    });
}

#[gpui_kit::test]
fn リンク先が無いwikiリンクはリンクにせず_押しても移らない(
    cx: &mut TestAppContext,
) {
    let (_dir, pages) = pages_with(&[("目次", "[[無いページ]]と[[目次]]")]);
    let (window, browser) = open_browser(cx, pages);
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            browser.read(cx).rendered_body(),
            Some("\\[\\[無いページ\\]\\]と[目次](nonoto-wikilink:0)"),
            "リンク先が無いWikiリンクをリンクにした"
        );
    });

    click_body_start(window, cx);
    assert_eq!(
        shown_title(window, cx).as_deref(),
        Some("目次"),
        "リンク先が無いWikiリンクを押して、ページが変わった"
    );
}

#[gpui_kit::test]
fn 保存してできたwikiリンクのリンク先を表示に反映する(
    cx: &mut TestAppContext,
) {
    let (_dir, pages) = pages_with(&[("はじめに", ""), ("目次", "")]);
    let (window, browser) = open_browser(cx, pages);
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        let state = browser.read(cx).body_input().clone();
        state.update(cx, |state, cx| state.set_value("[[はじめに]]", window, cx));
        window.click("save", cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();

    click_body_start(window, cx);
    assert_eq!(
        shown_title(window, cx).as_deref(),
        Some("はじめに"),
        "保存した本文のWikiリンクで、リンク先のページへ移らない"
    );
}

#[gpui_kit::test]
fn wikiリンクでないリンクは部品の既定と同じく開く(cx: &mut TestAppContext) {
    let (_dir, pages) = pages_with(&[("目次", "[外のページ](https://example.com/)")]);
    let (window, _browser) = open_browser(cx, pages);
    cx.run_until_parked();

    click_body_start(window, cx);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://example.com/"),
        "Wikiリンクでないリンクを開いていない"
    );
    assert_eq!(
        shown_title(window, cx).as_deref(),
        Some("目次"),
        "Wikiリンクでないリンクで、ページが変わった"
    );
}
