use std::path::Path;

use gpui_kit::test::{TestInput, TestQueryExt, TestWindowExt};
use gpui_kit::{
    AppContext, Bounds, ElementInputHandler, Entity, Point, TestAppContext, VisualTestContext,
    Window, WindowBounds, WindowHandle, WindowOptions, base::Root, px, size,
};
use nonoto_core::{KeyStorage, NewPage, Page, PageStore, Pages, SqlCipherStore};
use nonoto_desktop::PageBrowser;
use tempfile::TempDir;

mod common;
use common::FailingStore;

type Browser = PageBrowser<SqlCipherStore>;

/// macOSではCommand、それ以外ではControlを押すキーの文字列にする。
fn secondary(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("cmd-{key}")
    } else {
        format!("ctrl-{key}")
    }
}

fn open_pages(dir: &Path) -> Pages<SqlCipherStore> {
    nonoto_core::open(dir, KeyStorage::File).expect("ページを開けなかった")
}

/// 一時ディレクトリにページを作る。あとに作ったページほど一覧の上に並ぶ。
fn data_with(titles_and_bodies: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let mut pages = open_pages(dir.path());
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
    dir
}

/// 画面を閉じたあとの保存の結果を、別に開いたストアから読む。
fn stored_pages(dir: &Path) -> Vec<Page> {
    let pages = open_pages(dir);
    pages
        .list()
        .expect("一覧を読めなかった")
        .into_iter()
        .map(|summary| pages.get(summary.id).expect("ページを読めなかった"))
        .collect()
}

fn open_browser(cx: &mut TestAppContext, dir: &Path) -> (WindowHandle<Root>, Entity<Browser>) {
    open_browser_with(cx, open_pages(dir))
}

/// 保存に失敗するストアで画面を開く。
fn open_failing_browser(
    cx: &mut TestAppContext,
    dir: &Path,
) -> (WindowHandle<Root>, Entity<PageBrowser<FailingStore>>) {
    open_browser_with(cx, Pages::new(FailingStore::new(open_pages(dir))))
}

fn open_browser_with<S: PageStore + 'static>(
    cx: &mut TestAppContext,
    pages: Pages<S>,
) -> (WindowHandle<Root>, Entity<PageBrowser<S>>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        nonoto_desktop::init(cx);
    });
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

/// 本文の入力の部品に、IMEと同じ経路でテキストを送る。
fn body_input<'a>(
    browser: &Entity<Browser>,
    window: &'a mut Window,
    cx: &gpui_kit::App,
) -> TestInput<'a> {
    let state = browser.read(cx).body_input().clone();
    let bounds = window.find_by_label("本文").bounds();
    TestInput::new(ElementInputHandler::new(bounds, state), window)
}

/// タイトルの入力を、指定した文字列に置き換える。
fn replace_title(window: &mut Window, title: &str, cx: &mut gpui_kit::App) {
    window.click("title-input", cx);
    window.press(&secondary("a"), cx);
    window.input(title, cx);
}

#[gpui_kit::test]
fn 編集して保存すると保存した内容を表示する(cx: &mut TestAppContext) {
    let dir = data_with(&[("タイトル", "")]);
    let (window, browser) = open_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        replace_title(window, "新しいタイトル", cx);
        let mut input = body_input(&browser, window, cx);
        input.commit_text("# 見出し\n\n1行目\n2行目", cx);
        window.click("save", cx);

        assert_eq!(
            window.find("page-title").label(),
            Some("新しいタイトル"),
            "保存したタイトルが表示されていない"
        );
        assert!(
            window.try_find("save").is_none(),
            "保存したあとも編集が続いている"
        );
    })
    .expect("ウィンドウを更新できなかった");

    let stored = stored_pages(dir.path());
    assert_eq!(stored.len(), 1, "ページの数が変わった");
    assert_eq!(
        stored[0].title, "新しいタイトル",
        "タイトルが保存されていない"
    );
    assert_eq!(
        stored[0].body, "# 見出し\n\n1行目\n2行目",
        "複数行の本文が保存されていない"
    );
}

#[gpui_kit::test]
fn キーボードのショートカットで保存できる(cx: &mut TestAppContext) {
    let dir = data_with(&[("タイトル", "本文")]);
    let (window, browser) = open_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        replace_title(window, "ショートカットで保存", cx);
        window.press(&secondary("s"), cx);
    })
    .expect("ウィンドウを更新できなかった");

    cx.update(|cx| {
        assert!(
            !browser.read(cx).is_editing(),
            "保存したあとも編集が続いている"
        );
    });
    assert_eq!(
        stored_pages(dir.path())[0].title,
        "ショートカットで保存",
        "ショートカットで保存されていない"
    );
}

#[gpui_kit::test]
fn キャンセルすると編集した内容を捨てる(cx: &mut TestAppContext) {
    let dir = data_with(&[("タイトル", "本文")]);
    let (window, _) = open_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        replace_title(window, "捨てるタイトル", cx);
        window.click("cancel", cx);

        assert_eq!(
            window.find("page-title").label(),
            Some("タイトル"),
            "キャンセルしたのに編集した内容が表示された"
        );
    })
    .expect("ウィンドウを更新できなかった");

    assert_eq!(
        stored_pages(dir.path())[0].title,
        "タイトル",
        "キャンセルしたのに保存された"
    );
}

#[gpui_kit::test]
fn 新しいページを作ってタイトルを書き保存できる(cx: &mut TestAppContext) {
    let dir = data_with(&[("既存のページ", "本文")]);
    let (window, _) = open_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-page", cx);

        assert_eq!(
            window.find("title-input").focused(),
            Some(true),
            "新しいページのタイトルの入力にフォーカスが無い"
        );
        window.input("新しいページ", cx);
        window.click("save", cx);

        assert_eq!(
            window.find(("page", 0usize)).label(),
            Some("新しいページ"),
            "作ったページが一覧の先頭に無い"
        );
        assert_eq!(
            window.find(("page", 0usize)).selected(),
            Some(true),
            "作ったページが選ばれていない"
        );
    })
    .expect("ウィンドウを更新できなかった");

    let titles: Vec<String> = stored_pages(dir.path())
        .into_iter()
        .map(|page| page.title)
        .collect();
    assert_eq!(
        titles,
        ["新しいページ", "既存のページ"],
        "保存したページが期待と異なる"
    );
}

#[gpui_kit::test]
fn 編集中にほかのページを選ぶと保存してから移る(cx: &mut TestAppContext) {
    let dir = data_with(&[("1つめ", "本文1"), ("2つめ", "本文2")]);
    let (window, browser) = open_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        replace_title(window, "2つめ (編集)", cx);
        window.click(("page", 1usize), cx);
    })
    .expect("ウィンドウを更新できなかった");
    // 一覧の選択のイベントは、ウィンドウの更新を終えてから届く。
    cx.run_until_parked();

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("page-title").label(),
            Some("1つめ"),
            "選んだページへ移っていない"
        );
    })
    .expect("ウィンドウを更新できなかった");

    cx.update(|cx| {
        assert!(
            !browser.read(cx).is_editing(),
            "ほかのページへ移ったのに編集が続いている"
        );
    });
    let titles: Vec<String> = stored_pages(dir.path())
        .into_iter()
        .map(|page| page.title)
        .collect();
    assert!(
        titles.contains(&"2つめ (編集)".to_owned()),
        "移る前に編集した内容が保存されていない: {titles:?}"
    );
}

#[gpui_kit::test]
fn 変換中の文字は確定した文字に置き換わって保存される(
    cx: &mut TestAppContext,
) {
    let dir = data_with(&[("タイトル", "")]);
    let (window, browser) = open_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        let mut input = body_input(&browser, window, cx);

        input.compose_text(None, "にほんご", Some(4..4), cx);
        assert_eq!(
            input.marked_text_range(cx),
            Some(0..4),
            "変換中の文字の範囲が期待と異なる"
        );
        input.compose_text(None, "日本語", Some(3..3), cx);
        input.commit_text("日本語", cx);
        assert_eq!(
            input.marked_text_range(cx),
            None,
            "確定したのに変換中の範囲が残った"
        );

        window.click("save", cx);
    })
    .expect("ウィンドウを更新できなかった");

    assert_eq!(
        stored_pages(dir.path())[0].body,
        "日本語",
        "確定した文字が保存されていない"
    );
}

#[gpui_kit::test]
fn unmarkで確定した本文も別のページを選ぶと保存される(
    cx: &mut TestAppContext,
) {
    let dir = data_with(&[("移動先", "本文"), ("編集対象", "")]);
    let edited_id = stored_pages(dir.path())[0].id;
    let (window, browser) = open_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        let mut input = body_input(&browser, window, cx);
        input.compose_text(None, "にほんご", Some(4..4), cx);
        input.compose_text(None, "日本語", Some(3..3), cx);
        input.unmark_text(cx);
        assert_eq!(
            input.marked_text_range(cx),
            None,
            "確定したのに変換中の範囲が残った"
        );
        assert_eq!(
            browser.read(cx).body_input().read(cx).value(),
            "日本語",
            "確定した文字が入力に残っていない"
        );
        window.click(("page", 1usize), cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("page-title").label(),
            Some("移動先"),
            "選んだページへ移っていない"
        );
    })
    .expect("ウィンドウを更新できなかった");

    let saved = open_pages(dir.path())
        .get(edited_id)
        .expect("編集したページを読めなかった");
    assert_eq!(
        saved.body, "日本語",
        "移る前に確定した本文が保存されていない"
    );
}

#[gpui_kit::test]
fn 無変更や元に戻した編集ではページを移っても更新しない(
    cx: &mut TestAppContext,
) {
    let dir = data_with(&[("1つめ", "本文1"), ("2つめ", "本文2")]);
    let before = stored_pages(dir.path());
    let (window, browser) = open_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        window.click(("page", 1usize), cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();

    assert_eq!(
        stored_pages(dir.path()),
        before,
        "無変更のページの内容や更新日時が変わった"
    );

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("page-title").label(),
            Some("1つめ"),
            "選んだページへ移っていない"
        );
        window.click("edit", cx);
        window.input("追加の本文", cx);
        window.press(&secondary("z"), cx);
        assert_eq!(
            browser.read(cx).body_input().read(cx).value(),
            "本文1",
            "編集した本文が元に戻っていない"
        );
        window.click(("page", 0usize), cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();

    cx.update(|cx| {
        assert!(
            !browser.read(cx).is_editing(),
            "移動したあとも編集が続いている"
        );
    });
    assert_eq!(
        stored_pages(dir.path()),
        before,
        "元の内容に戻したページの内容や更新日時が変わった"
    );
}

/// 表示しているエラーの文言。
fn error_label(window: &Window) -> Option<String> {
    window
        .try_find("error")
        .and_then(|error| error.label().map(str::to_owned))
}

#[gpui_kit::test]
fn 保存できなければ別のページへ移らず_キャンセルするとエラーを消す(
    cx: &mut TestAppContext,
) {
    let dir = data_with(&[("1つめ", "本文1"), ("2つめ", "本文2")]);
    let before = stored_pages(dir.path());
    let (window, browser) = open_failing_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        replace_title(window, "変えたタイトル", cx);
        window.click(("page", 1usize), cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            browser.read(cx).is_editing(),
            "保存できないのに編集が終わった"
        );
        assert_eq!(
            browser.read(cx).title_input().read(cx).value(),
            "変えたタイトル",
            "保存できなかった入力が消えた"
        );
        assert_eq!(
            window.find(("page", 0usize)).selected(),
            Some(true),
            "保存できないのに別のページへ移った"
        );
        assert!(
            error_label(window).is_some_and(|label| label.contains("ページを更新できない")),
            "保存できない旨が表示されない"
        );

        // 編集中は「新しいページ」のボタンを出さないため、ボタンを押す代わりに直接呼ぶ。
        browser.update(cx, |browser, cx| browser.new_page(window, cx));
        window.render_frame(cx);
        assert!(
            browser.read(cx).is_editing(),
            "保存できないのに編集を終えて新しいページを作ろうとした"
        );
        assert_eq!(
            browser.read(cx).title_input().read(cx).value(),
            "変えたタイトル",
            "新しいページを押したら保存できなかった入力が消えた"
        );
        assert!(
            error_label(window).is_some_and(|label| label.contains("ページを更新できない")),
            "新しいページを押したら保存できない旨が消えた"
        );

        window.click("cancel", cx);
        window.render_frame(cx);
        assert!(
            !browser.read(cx).is_editing(),
            "キャンセルしても編集が終わらない"
        );
        assert_eq!(
            error_label(window),
            None,
            "キャンセルしても保存のエラーが残っている"
        );
        assert_eq!(
            window.find("page-title").label(),
            Some("2つめ"),
            "キャンセルしたあとに編集していたページを表示していない"
        );
    })
    .expect("ウィンドウを更新できなかった");

    assert_eq!(
        stored_pages(dir.path()),
        before,
        "保存できないのにページが変わった"
    );
}

#[gpui_kit::test]
fn 編集中にウィンドウを閉じると変えた内容を保存する(
    cx: &mut TestAppContext,
) {
    let dir = data_with(&[("タイトル", "本文")]);
    let (window, _) = open_browser(cx, dir.path());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.simulate_close(),
        "編集していないのにウィンドウを閉じられない"
    );

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        replace_title(window, "閉じる前のタイトル", cx);
    })
    .expect("ウィンドウを更新できなかった");

    assert!(
        visual.simulate_close(),
        "保存できたのにウィンドウを閉じられない"
    );
    assert_eq!(
        stored_pages(dir.path())[0].title,
        "閉じる前のタイトル",
        "閉じる前に変えた内容が保存されていない"
    );
}

#[gpui_kit::test]
fn 保存できなければウィンドウを閉じない(cx: &mut TestAppContext) {
    let dir = data_with(&[("タイトル", "本文")]);
    let (window, browser) = open_failing_browser(cx, dir.path());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        replace_title(window, "変えたタイトル", cx);
    })
    .expect("ウィンドウを更新できなかった");

    assert!(
        !VisualTestContext::from_window(window.into(), cx).simulate_close(),
        "保存できないのにウィンドウを閉じた"
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            browser.read(cx).is_editing(),
            "保存できないのに編集が終わった"
        );
        assert!(
            error_label(window).is_some_and(|label| label.contains("ページを更新できない")),
            "保存できない旨が表示されない"
        );
    })
    .expect("ウィンドウを更新できなかった");
}
