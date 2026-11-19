use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::component::input::{CompletionProvider, InputEdit, InputEditsEvent, Rope};
use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::test::{TestInput, TestQueryExt, TestWindowExt};
use gpui_kit::{
    AppContext, Bounds, ClipboardItem, ElementInputHandler, Entity, Hsla, Point, Subscription,
    Task, TestAppContext, VisualTestContext, Window, WindowBounds, WindowHandle, WindowOptions,
    base::Root, px, size,
};
use nonoto_core::{KeyStorage, NewPage, Page, PageStore, Pages, SqlCipherStore};
use nonoto_desktop::PageBrowser;
use tempfile::TempDir;

mod common;
use common::{FailingStore, SteppingClock};

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
    nonoto_core::open_with_clock(dir, KeyStorage::File, SteppingClock::new())
        .expect("ページを開けなかった")
}

/// 一時ディレクトリにページを作り、作成に使った時計を持つPagesを返す。
fn data_with(titles_and_bodies: &[(&str, &str)]) -> (TempDir, Pages<SqlCipherStore>) {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let mut pages = open_pages(dir.path());
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

/// 保存に失敗するストアで画面を開く。
fn open_failing_browser(
    cx: &mut TestAppContext,
    pages: Pages<SqlCipherStore>,
) -> (WindowHandle<Root>, Entity<PageBrowser<FailingStore>>) {
    open_browser_with(
        cx,
        Pages::with_clock(FailingStore::new(pages), SteppingClock::new()),
    )
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
    let (dir, pages) = data_with(&[("タイトル", "")]);
    let (window, browser) = open_browser_with(cx, pages);

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
    let (dir, pages) = data_with(&[("タイトル", "本文")]);
    let (window, browser) = open_browser_with(cx, pages);

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
    let (dir, pages) = data_with(&[("タイトル", "本文")]);
    let (window, _) = open_browser_with(cx, pages);

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
    let (dir, pages) = data_with(&[("既存のページ", "本文")]);
    let (window, _) = open_browser_with(cx, pages);

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
    let (dir, pages) = data_with(&[("1つめ", "本文1"), ("2つめ", "本文2")]);
    let (window, browser) = open_browser_with(cx, pages);

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
    let (dir, pages) = data_with(&[("タイトル", "")]);
    let (window, browser) = open_browser_with(cx, pages);

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
    let (dir, pages) = data_with(&[("移動先", "本文"), ("編集対象", "")]);
    let edited_id = stored_pages(dir.path())[0].id;
    let (window, browser) = open_browser_with(cx, pages);

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
    let (dir, pages) = data_with(&[("1つめ", "本文1"), ("2つめ", "本文2")]);
    let before = stored_pages(dir.path());
    let (window, browser) = open_browser_with(cx, pages);

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
    let (dir, pages) = data_with(&[("1つめ", "本文1"), ("2つめ", "本文2")]);
    let before = stored_pages(dir.path());
    let (window, browser) = open_failing_browser(cx, pages);

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
    let (dir, pages) = data_with(&[("タイトル", "本文")]);
    let (window, _) = open_browser_with(cx, pages);
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
    let (_dir, pages) = data_with(&[("タイトル", "本文")]);
    let (window, browser) = open_failing_browser(cx, pages);

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

#[gpui_kit::test]
fn 本文の入力はmarkdownの記法を装飾する(cx: &mut TestAppContext) {
    let (_dir, pages) = data_with(&[("タイトル", "本文")]);
    let (window, browser) = open_browser_with(cx, pages);

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        assert_eq!(
            browser
                .read(cx)
                .body_input()
                .read(cx)
                .language_name()
                .as_ref(),
            nonoto_desktop::markdown_highlight::LANGUAGE,
            "本文の入力の言語がMarkdownでない"
        );
    })
    .expect("ウィンドウを更新できなかった");
}

/// テーマのハイライトで、記法の記号に使う色。
fn punctuation_color(cx: &gpui_kit::App) -> Option<Hsla> {
    cx.theme()
        .highlight_theme
        .style("punctuation")
        .and_then(|style| style.color)
}

#[gpui_kit::test]
fn ライトとダークを切り替えても記法の装飾を保つ(cx: &mut TestAppContext) {
    let (_dir, pages) = data_with(&[("タイトル", "本文")]);
    open_browser_with(cx, pages);

    cx.update(|cx| {
        let light = punctuation_color(cx);
        assert!(light.is_some(), "ライトのテーマに記号の色が無い");

        Theme::change(ThemeMode::Dark, None, cx);
        let dark = punctuation_color(cx);
        assert!(dark.is_some(), "ダークに切り替えたら記号の色が無くなった");
        assert_ne!(light, dark, "ライトとダークで記号の色が同じ");

        Theme::change(ThemeMode::Light, None, cx);
        assert_eq!(
            punctuation_color(cx),
            light,
            "ライトに戻したら記号の色が変わった"
        );
    });
}

/// Wikiリンクとして装飾している範囲の本文。
fn decorated_texts(browser: &Entity<Browser>, cx: &gpui_kit::App) -> Vec<String> {
    let browser = browser.read(cx);
    let body = browser.body_input().read(cx).value();
    browser
        .wikilink_decoration_ranges(cx)
        .into_iter()
        .map(|range| body[range].to_owned())
        .collect()
}

#[gpui_kit::test]
fn 編集を始めるとwikiリンクを記号とタイトルに分けて装飾する(
    cx: &mut TestAppContext,
) {
    let (_dir, pages) = data_with(&[("タイトル", "前に[[はじめに]]と`[[コード]]`がある")]);
    let (window, browser) = open_browser_with(cx, pages);

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        assert_eq!(
            decorated_texts(&browser, cx),
            ["[[", "はじめに", "]]"],
            "Wikiリンクの装飾が期待と異なる"
        );
    })
    .expect("ウィンドウを更新できなかった");
}

/// 本文の入力に、IMEと同じ経路で確定した文字を送る。入力の通知が画面に届くまで待つ。
fn commit_body(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    browser: &Entity<Browser>,
    text: &str,
) {
    cx.update_window(window.into(), |_, window, cx| {
        body_input(browser, window, cx).commit_text(text, cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
}

#[gpui_kit::test]
fn 編集してもwikiリンクの装飾が本文の位置とずれない(cx: &mut TestAppContext) {
    let (_dir, pages) = data_with(&[("タイトル", "[[はじめに]]")]);
    let (window, browser) = open_browser_with(cx, pages);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
    })
    .expect("ウィンドウを更新できなかった");

    // 編集を始めたときのカーソルは本文の先頭にある。
    commit_body(cx, window, &browser, "前に足した文。");
    cx.update(|cx| {
        assert_eq!(
            decorated_texts(&browser, cx),
            ["[[", "はじめに", "]]"],
            "前に文字を足したら装飾がずれた"
        );
    });

    commit_body(cx, window, &browser, "[[新しい]]");
    cx.update(|cx| {
        assert_eq!(
            decorated_texts(&browser, cx),
            ["[[", "新しい", "]]", "[[", "はじめに", "]]"],
            "入力したWikiリンクが装飾されていない"
        );
    });

    cx.update_window(window.into(), |_, window, cx| window.press("backspace", cx))
        .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
    cx.update(|cx| {
        // 閉じる記号の片方を消すと、最初の `[[` から次の `]]` までが1つのWikiリンクになる。
        assert_eq!(
            decorated_texts(&browser, cx),
            ["[[", "新しい][[はじめに", "]]"],
            "閉じる記号を消したあとのWikiリンクの装飾が違う"
        );
    });
}

#[gpui_kit::test]
fn 変換中の文字を足してもwikiリンクの装飾がずれない(cx: &mut TestAppContext) {
    let (_dir, pages) = data_with(&[("タイトル", "[[はじめに]]")]);
    let (window, browser) = open_browser_with(cx, pages);

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        let mut input = body_input(&browser, window, cx);

        input.compose_text(None, "にほんご", Some(4..4), cx);
        assert_eq!(
            decorated_texts(&browser, cx),
            ["[[", "はじめに", "]]"],
            "変換中に装飾がずれた"
        );
        input.commit_text("日本語", cx);
        assert_eq!(
            decorated_texts(&browser, cx),
            ["[[", "はじめに", "]]"],
            "確定したら装飾がずれた"
        );
    })
    .expect("ウィンドウを更新できなかった");
}

/// 本文の入力の今の本文。
fn body_text(browser: &Entity<Browser>, cx: &gpui_kit::App) -> String {
    browser.read(cx).body_input().read(cx).value().to_string()
}

/// 本文の入力で、選ぶ範囲を指定する。
fn select_body(browser: &Entity<Browser>, range: std::ops::Range<usize>, cx: &mut gpui_kit::App) {
    let state = browser.read(cx).body_input().clone();
    state.update(cx, |state, cx| state.set_selected_range(range, cx));
}

/// 本文の入力で、キーを押す。押したあとの通知が画面に届くまで待つ。
fn press_in_body(cx: &mut TestAppContext, window: WindowHandle<Root>, key: &str) {
    cx.update_window(window.into(), |_, window, cx| window.press(key, cx))
        .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
}

/// 本文を持つページを開いて編集を始め、カーソルを本文の末尾に置く。
fn edit_body_at_end(
    cx: &mut TestAppContext,
    body: &str,
) -> (TempDir, WindowHandle<Root>, Entity<Browser>) {
    let (dir, pages) = data_with(&[("タイトル", body)]);
    let (window, browser) = open_browser_with(cx, pages);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        select_body(&browser, body.len()..body.len(), cx);
    })
    .expect("ウィンドウを更新できなかった");
    (dir, window, browser)
}

#[gpui_kit::test]
fn enterでリストを続け_空の項目で終わり_取り消しで戻る(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_body_at_end(cx, "- [x] 済み");

    press_in_body(cx, window, "enter");
    commit_body(cx, window, &browser, "次");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- [x] 済み\n- [ ] 次",
            "Enterでリストを続けていない"
        );
    });

    press_in_body(cx, window, "enter");
    press_in_body(cx, window, "enter");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- [x] 済み\n- [ ] 次\n",
            "空の項目でEnterを押してもリストが終わらない"
        );
    });

    press_in_body(cx, window, &secondary("z"));
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- [x] 済み\n- [ ] 次\n- [ ] ",
            "取り消しで、記号を消す前に戻らない"
        );
    });
}

#[gpui_kit::test]
fn リストでない行やshift_enterではふつうに改行する(cx: &mut TestAppContext) {
    let (_dir, window, browser) = edit_body_at_end(cx, "ふつうの文");

    press_in_body(cx, window, "enter");
    commit_body(cx, window, &browser, "- 項目");
    press_in_body(cx, window, "shift-enter");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "ふつうの文\n- 項目\n",
            "リストを続けない改行が期待と異なる"
        );
    });
}

#[gpui_kit::test]
fn 変換中のenterではリストを続けない(cx: &mut TestAppContext) {
    let (_dir, window, browser) = edit_body_at_end(cx, "- ");

    cx.update_window(window.into(), |_, window, cx| {
        body_input(&browser, window, cx).compose_text(None, "にほんご", Some(4..4), cx);
    })
    .expect("ウィンドウを更新できなかった");
    press_in_body(cx, window, "enter");
    cx.update(|cx| {
        assert!(
            !body_text(&browser, cx).contains("\n- "),
            "変換中のEnterでリストを続けた: {:?}",
            body_text(&browser, cx)
        );
    });
}

#[gpui_kit::test]
fn tabとshift_tabで選んだ複数の行の字下げを変える(cx: &mut TestAppContext) {
    let body = "- a\n- b\n- c";
    let (_dir, window, browser) = edit_body_at_end(cx, body);
    cx.update(|cx| select_body(&browser, 2..7, cx));

    press_in_body(cx, window, "tab");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "  - a\n  - b\n- c",
            "Tabで選んだ行に字下げを足していない"
        );
    });

    press_in_body(cx, window, "shift-tab");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            body,
            "Shift-Tabで選んだ行の字下げを削っていない"
        );
    });
}

#[gpui_kit::test]
fn 括弧の自動閉じと取り消しとやり直しと検索を使える(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_body_at_end(cx, "");

    commit_body(cx, window, &browser, "(");
    cx.update(|cx| {
        assert_eq!(body_text(&browser, cx), "()", "括弧を自動で閉じていない");
    });

    press_in_body(cx, window, &secondary("z"));
    cx.update(|cx| assert_eq!(body_text(&browser, cx), "", "取り消せていない"));
    let redo = if cfg!(target_os = "macos") {
        "cmd-shift-z"
    } else {
        "ctrl-y"
    };
    press_in_body(cx, window, redo);
    cx.update(|cx| assert_eq!(body_text(&browser, cx), "()", "やり直せていない"));

    press_in_body(cx, window, &secondary("f"));
    cx.update(|cx| {
        assert!(
            browser
                .read(cx)
                .body_input()
                .read(cx)
                .search_session()
                .is_active(),
            "検索を開けていない"
        );
    });
}

#[gpui_kit::test]
fn shift_tabで変更不要なら選択外の行を変えない(cx: &mut TestAppContext) {
    let body = "a\n    b";
    let (_dir, window, browser) = edit_body_at_end(cx, body);
    cx.update(|cx| select_body(&browser, 0..2, cx));

    press_in_body(cx, window, "shift-tab");
    cx.update(|cx| {
        assert_eq!(body_text(&browser, cx), body, "選択していない行を変えた");
        assert_eq!(
            browser.read(cx).body_input().read(cx).selected_range(),
            0..2,
            "変更不要の選択を変えた"
        );
    });
}

#[gpui_kit::test]
fn 字下げを取り消すと元のカーソルへ戻る(cx: &mut TestAppContext) {
    let body = "前の行\n- 本文";
    let cursor = body.find("本文").expect("本文が無い");
    let (_dir, window, browser) = edit_body_at_end(cx, body);
    cx.update(|cx| select_body(&browser, cursor..cursor, cx));

    press_in_body(cx, window, "tab");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "前の行\n  - 本文",
            "本文の位置と字下げが違う"
        );
    });
    press_in_body(cx, window, &secondary("z"));
    cx.update(|cx| {
        assert_eq!(body_text(&browser, cx), body, "本文が元に戻らない");
        assert_eq!(
            browser.read(cx).body_input().read(cx).selected_range(),
            cursor..cursor,
            "カーソルが元に戻らない"
        );
    });
}

#[gpui_kit::test]
fn 複数行の字下げを取り消すと元の選択へ戻る(cx: &mut TestAppContext) {
    let body = "- a\n- b\n- c";
    let (_dir, window, browser) = edit_body_at_end(cx, body);
    cx.update(|cx| select_body(&browser, 2..7, cx));

    press_in_body(cx, window, "tab");
    press_in_body(cx, window, &secondary("z"));
    cx.update(|cx| {
        assert_eq!(body_text(&browser, cx), body, "本文が元に戻らない");
        assert_eq!(
            browser.read(cx).body_input().read(cx).selected_range(),
            2..7,
            "選択が元に戻らない"
        );
    });
}

#[gpui_kit::test]
fn 入力直後のtabは入力と分けて取り消せる(cx: &mut TestAppContext) {
    for text in ["本文", "- "] {
        let (_dir, window, browser) = edit_body_at_end(cx, "");
        commit_body(cx, window, &browser, text);
        press_in_body(cx, window, "tab");
        press_in_body(cx, window, &secondary("z"));
        cx.update(|cx| {
            assert_eq!(
                body_text(&browser, cx),
                text,
                "{text:?} の入力まで取り消した"
            );
            assert_eq!(
                browser.read(cx).body_input().read(cx).selected_range(),
                text.len()..text.len(),
                "{text:?} の入力の後ろのカーソルへ戻らない"
            );
        });
    }
}

/// 補完の判定に渡った位置と文字列を記録し、候補は出さない補完のプロバイダー。
struct RecordingCompletion(Rc<RefCell<Vec<(usize, String)>>>);

impl CompletionProvider for RecordingCompletion {
    fn completions(
        &self,
        _text: &Rope,
        _offset: usize,
        _trigger: lsp_types::CompletionContext,
        _window: &mut Window,
        _cx: &mut gpui_kit::App,
    ) -> Task<gpui_kit::Result<lsp_types::CompletionResponse>> {
        Task::ready(Ok(lsp_types::CompletionResponse::Array(Vec::new())))
    }

    fn is_completion_trigger(
        &self,
        offset: usize,
        new_text: &str,
        _cx: &mut gpui_kit::App,
    ) -> bool {
        self.0.borrow_mut().push((offset, new_text.to_owned()));
        false
    }
}

#[gpui_kit::test]
fn 字下げとリストの編集では補完の判定を呼ばない(cx: &mut TestAppContext) {
    // どれも、置き換える範囲がカーソルと異なる編集である。
    for (body, key) in [("- ", "tab"), ("  - ", "enter"), ("    - [[a", "shift-tab")] {
        let (_dir, window, browser) = edit_body_at_end(cx, body);
        let calls = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(RecordingCompletion(calls.clone()));
        cx.update(|cx| {
            let state = browser.read(cx).body_input().clone();
            state.update(cx, |state, _| {
                state.lsp_mut().completion_provider = Some(provider);
            });
        });

        press_in_body(cx, window, key);
        cx.update(|cx| {
            assert_ne!(
                body_text(&browser, cx),
                body,
                "{key} で {body:?} を編集していない"
            );
        });
        assert!(
            calls.borrow().is_empty(),
            "{key} で {body:?} を編集すると補完の判定を呼んだ: {:?}",
            calls.borrow()
        );

        commit_body(cx, window, &browser, "x");
        assert_eq!(
            calls.borrow().len(),
            1,
            "{key} で {body:?} を編集したあと、文字を打っても補完の判定を呼ばない"
        );
    }
}

/// 本文の入力で出している補完の候補のタイトル。候補を出していなければ空にする。
fn completion_labels(browser: &Entity<Browser>, cx: &gpui_kit::App) -> Vec<String> {
    let menu = browser
        .read(cx)
        .body_input()
        .read(cx)
        .completion_menu_state();
    if !menu.open {
        return Vec::new();
    }
    menu.items.iter().map(|item| item.label.clone()).collect()
}

/// 補完の候補に使うページと、編集する本文のページを作って編集を始める。カーソルは本文の末尾に置く。
fn edit_with_pages(
    cx: &mut TestAppContext,
    body: &str,
) -> (TempDir, WindowHandle<Root>, Entity<Browser>) {
    let (dir, pages) = data_with(&[
        ("はじめに", ""),
        ("議事録", ""),
        ("はじめての設定", ""),
        ("編集するページ", body),
    ]);
    let (window, browser) = open_browser_with(cx, pages);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
        select_body(&browser, body.len()..body.len(), cx);
    })
    .expect("ウィンドウを更新できなかった");
    (dir, window, browser)
}

#[gpui_kit::test]
fn 開く記号の後ろで候補を出し_キーワードで絞り_選ぶとwikiリンクが入る(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, "前に");

    commit_body(cx, window, &browser, "[");
    commit_body(cx, window, &browser, "[");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "前に[[]]",
            "括弧を自動で閉じていない"
        );
        assert_eq!(
            completion_labels(&browser, cx),
            ["編集するページ", "はじめての設定", "議事録", "はじめに"],
            "キーワードが空のときに、更新の新しい順にページを出していない"
        );
    });

    commit_body(cx, window, &browser, "はじ");
    cx.update(|cx| {
        assert_eq!(
            completion_labels(&browser, cx),
            ["はじめての設定", "はじめに"],
            "キーワードで候補を絞っていない"
        );
    });

    press_in_body(cx, window, "down");
    press_in_body(cx, window, "enter");
    commit_body(cx, window, &browser, "後");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "前に[[はじめに]]後",
            "選んだ候補がWikiリンクとして入っていない"
        );
        assert!(
            completion_labels(&browser, cx).is_empty(),
            "選んだあとも候補を出している"
        );
        assert_eq!(
            decorated_texts(&browser, cx),
            ["[[", "はじめに", "]]"],
            "入ったWikiリンクが装飾されていない"
        );
    });
}

#[gpui_kit::test]
fn リストの項目の中でも_候補を出しているときのenterは候補を選ぶ(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, "- [[議");
    commit_body(cx, window, &browser, "事");
    cx.update(|cx| {
        assert_eq!(
            completion_labels(&browser, cx),
            ["議事録"],
            "候補を出していない"
        );
    });

    press_in_body(cx, window, "enter");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- [[議事録]]",
            "Enterで候補を選ばず、リストを続けた"
        );
    });

    press_in_body(cx, window, "enter");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- [[議事録]]\n- ",
            "候補を閉じたあとのEnterでリストを続けていない"
        );
    });
}

#[gpui_kit::test]
fn 候補を出したまま変換しても入力を壊さず_確定したあとに候補を出す(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, "[[");
    commit_body(cx, window, &browser, "は");
    cx.update(|cx| {
        assert_eq!(
            completion_labels(&browser, cx),
            ["はじめての設定", "はじめに"],
            "変換を始める前に候補を出していない"
        );
    });

    cx.update_window(window.into(), |_, window, cx| {
        body_input(&browser, window, cx).compose_text(None, "じめに", Some(3..3), cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        let mut input = body_input(&browser, window, cx);
        assert_eq!(
            input.marked_text_range(cx),
            Some(3..6),
            "変換中の文字の範囲が崩れた"
        );
        assert_eq!(
            body_text(&browser, cx),
            "[[はじめに",
            "変換中の文字が崩れた"
        );
        input.commit_text("じめに", cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "[[はじめに",
            "確定した文字が入っていない"
        );
        assert_eq!(
            completion_labels(&browser, cx),
            ["はじめに"],
            "確定したあとに候補を出していない"
        );
    });
}

#[gpui_kit::test]
fn 閉じる記号を打つと候補を閉じる(cx: &mut TestAppContext) {
    let (_dir, window, browser) = edit_with_pages(cx, "[[議事録");
    commit_body(cx, window, &browser, "]");
    commit_body(cx, window, &browser, "]");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "[[議事録]]",
            "閉じる記号が入っていない"
        );
        assert!(
            completion_labels(&browser, cx).is_empty(),
            "Wikiリンクを閉じても候補を出している"
        );
    });
}

#[gpui_kit::test]
fn wikiリンクとして表せるタイトルだけを補完する(cx: &mut TestAppContext) {
    let (_dir, pages) = data_with(&[
        ("　", ""),
        ("議事録]]追記", ""),
        (" はじめに", ""),
        ("はじめに", ""),
        ("編集中", ""),
    ]);
    let (window, browser) = open_browser_with(cx, pages);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("edit", cx);
    })
    .expect("ウィンドウを更新できなかった");
    commit_body(cx, window, &browser, "[");
    commit_body(cx, window, &browser, "[");
    cx.update(|cx| {
        assert_eq!(
            completion_labels(&browser, cx),
            ["編集中", "はじめに"],
            "元のタイトルとして表せない候補がある"
        );
    });

    press_in_body(cx, window, "down");
    press_in_body(cx, window, "enter");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "[[はじめに]]",
            "通常のタイトルを補完できない"
        );
    });
}

#[gpui_kit::test]
fn unmarkで確定したあとも明示的な要求なら候補を出せる(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, "[[");
    cx.update_window(window.into(), |_, window, cx| {
        let mut input = body_input(&browser, window, cx);
        input.compose_text(None, "議事録", Some(3..3), cx);
        input.unmark_text(cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();

    // unmark_textは補完の通知を出さない。確定後に公開APIで要求すれば候補を出せることを検証する。
    cx.update_window(window.into(), |_, window, cx| {
        browser
            .read(cx)
            .body_input()
            .clone()
            .update(cx, |state, cx| {
                state.show_completions(window, cx);
            });
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "[[議事録",
            "確定した本文が変わった"
        );
        assert_eq!(
            completion_labels(&browser, cx),
            ["議事録"],
            "明示的に候補を要求できない"
        );
    });
}

#[gpui_kit::test]
fn unmarkで確定したあとの古い候補は明示的な要求なら更新できる(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, "[[");
    commit_body(cx, window, &browser, "は");
    cx.update_window(window.into(), |_, window, cx| {
        let mut input = body_input(&browser, window, cx);
        input.compose_text(None, "じめに", Some(3..3), cx);
        input.unmark_text(cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();

    // 開いている候補にはshow_completionsが何もしないため、公開APIで閉じてから要求する。
    cx.update_window(window.into(), |_, window, cx| {
        browser
            .read(cx)
            .body_input()
            .clone()
            .update(cx, |state, cx| {
                state.dismiss_completion_overlay(cx);
                state.show_completions(window, cx);
            });
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "[[はじめに",
            "確定した本文が変わった"
        );
        assert_eq!(
            completion_labels(&browser, cx),
            ["はじめに"],
            "明示的に候補を更新できない"
        );
    });
}

/// 画面を描画して、待っている処理を終えるまで進める。
///
/// `press` はキーを送る直前にも描画するが、利用者がキーを押すときの画面は、描画のあとの処理も終えて落ち着いている。
fn settle(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
}

/// 本文の入力に文字を順に打ち、打つたびに画面が落ち着くまで進める。
fn commit_body_settled(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    browser: &Entity<Browser>,
    texts: &[&str],
) {
    for text in texts {
        commit_body(cx, window, browser, text);
        settle(cx, window);
    }
}

#[gpui_kit::test]
fn 落ち着いた画面でも_リストの項目の中でenterで選ぶと候補だけが入る(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, "- ");
    commit_body_settled(cx, window, &browser, &["[", "["]);
    press_in_body(cx, window, "enter");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- [[編集するページ]]",
            "候補を開いたまま選んだEnterで、リストも続けた"
        );
    });

    let (_dir, window, browser) = edit_with_pages(cx, "- ");
    commit_body_settled(cx, window, &browser, &["[", "[", "はじ"]);
    press_in_body(cx, window, "down");
    settle(cx, window);
    press_in_body(cx, window, "enter");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- [[はじめに]]",
            "絞り込んで選んだEnterで、リストも続けた"
        );
    });
}

#[gpui_kit::test]
fn 落ち着いた画面でも_下矢印で次の候補を選び_enterで改行を入れない(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, "[[はじ");
    commit_body_settled(cx, window, &browser, &["め"]);
    cx.update(|cx| {
        assert_eq!(
            completion_labels(&browser, cx),
            ["はじめての設定", "はじめに"],
            "候補を出していない"
        );
    });

    press_in_body(cx, window, "down");
    settle(cx, window);
    press_in_body(cx, window, "enter");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "[[はじめに]]",
            "下矢印で次の候補を選べないか、Enterで改行も入れた"
        );
    });
}

#[gpui_kit::test]
fn 候補を出しているときもshift_tabはコアの規則で字下げを削り_候補を閉じる(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, " - [[は");
    commit_body_settled(cx, window, &browser, &["じ"]);
    cx.update(|cx| {
        assert!(
            !completion_labels(&browser, cx).is_empty(),
            "候補を出していない"
        );
    });

    press_in_body(cx, window, "shift-tab");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- [[はじ",
            "行頭の空白を1つだけ持つ行で、字下げを削っていない"
        );
        assert!(
            completion_labels(&browser, cx).is_empty(),
            "字下げを削ったあとも候補を出している"
        );
    });
}

/// 本文の入力で出している候補ごとに、強調するタイトルの部分。
fn completion_highlights(browser: &Entity<Browser>, cx: &gpui_kit::App) -> Vec<Vec<String>> {
    let state = browser.read(cx).body_input().read(cx);
    let menu = state.completion_menu_state();
    let highlight = state.lsp().completion_highlighter().clone();
    menu.items
        .iter()
        .map(|item| {
            highlight(&menu.query, item)
                .into_iter()
                .map(|range| item.label[range].to_owned())
                .collect()
        })
        .collect()
}

#[gpui_kit::test]
fn 書きかけのwikiリンクに打ち足しても_探したキーワードの語を強調する(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, "[[はじ");
    commit_body(cx, window, &browser, "め");
    cx.update(|cx| {
        assert_eq!(
            completion_labels(&browser, cx),
            ["はじめての設定", "はじめに"],
            "候補を出していない"
        );
        assert_eq!(
            completion_highlights(&browser, cx),
            [["はじめ"], ["はじめ"]],
            "打ち足した文字だけを強調した"
        );
    });

    let (_dir, window, browser) = edit_with_pages(cx, "[[設定 は");
    commit_body(cx, window, &browser, "じ");
    cx.update(|cx| {
        assert_eq!(
            completion_highlights(&browser, cx),
            [["はじ", "設定"]],
            "キーワードの語ごとに、タイトルの中の位置の順で強調していない"
        );
    });
}

/// 本文の入力が出す差分のイベントを、購読を始めたときの本文に順に当てたもの。
///
/// 差分で本文を追いかける利用者 (保存や同期) と同じく、差分の位置と、差分のあとの本文から切り出した文字だけを使う。
struct ReplayedBody {
    text: Rc<RefCell<String>>,
    edits: Rc<RefCell<Vec<InputEdit>>>,
    _subscription: Subscription,
}

impl ReplayedBody {
    /// 本文の入力の差分の購読を始める。編集を始めたあとに呼ぶ。
    fn subscribe(browser: &Entity<Browser>, cx: &mut TestAppContext) -> Self {
        cx.update(|cx| {
            let state = browser.read(cx).body_input().clone();
            let text = Rc::new(RefCell::new(state.read(cx).value().to_string()));
            let edits = Rc::new(RefCell::new(Vec::new()));
            let (replayed, received) = (text.clone(), edits.clone());
            let subscription = cx.subscribe(&state, move |_, event: &InputEditsEvent, _| {
                for (edit, after) in event.edits() {
                    let inserted = after.slice(edit.start_byte..edit.new_end_byte).to_string();
                    replayed
                        .borrow_mut()
                        .replace_range(edit.start_byte..edit.old_end_byte, &inserted);
                    received.borrow_mut().push(*edit);
                }
            });
            Self {
                text,
                edits,
                _subscription: subscription,
            }
        })
    }

    fn text(&self) -> String {
        self.text.borrow().clone()
    }

    /// 差分を当てた本文が、入力の本文と一致することを確かめる。
    fn assert_matches(&self, browser: &Entity<Browser>, cx: &mut TestAppContext, operation: &str) {
        cx.update(|cx| {
            assert_eq!(
                self.text(),
                body_text(browser, cx),
                "{operation}の差分を当てた本文が、入力の本文と一致しない"
            );
        });
    }
}

#[gpui_kit::test]
fn 文字の入力と削除と貼り付けの差分を当てると入力の本文になる(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_body_at_end(cx, "あいう\nかき");
    let replayed = ReplayedBody::subscribe(&browser, cx);

    commit_body(cx, window, &browser, "くけ");
    replayed.assert_matches(&browser, cx, "文字の入力");
    commit_body(cx, window, &browser, "(");
    replayed.assert_matches(&browser, cx, "自動で閉じる括弧の入力");
    press_in_body(cx, window, "backspace");
    replayed.assert_matches(&browser, cx, "後ろへの削除");

    cx.update(|cx| select_body(&browser, 3..3, cx));
    press_in_body(cx, window, "delete");
    replayed.assert_matches(&browser, cx, "前への削除");

    cx.update(|cx| select_body(&browser, 0..6, cx));
    cx.write_to_clipboard(ClipboardItem::new_string("貼った\n2行".to_owned()));
    press_in_body(cx, window, &secondary("v"));
    replayed.assert_matches(&browser, cx, "選んだ範囲への貼り付け");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "貼った\n2行\nかきくけ",
            "操作のあとの本文が期待と異なる"
        );
    });
}

#[gpui_kit::test]
fn 取り消しとやり直しの差分を当てると入力の本文になる(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_body_at_end(cx, "- a\n- b");
    let replayed = ReplayedBody::subscribe(&browser, cx);

    commit_body(cx, window, &browser, "c");
    cx.update(|cx| select_body(&browser, 0..7, cx));
    press_in_body(cx, window, "tab");
    replayed.assert_matches(&browser, cx, "複数の行の字下げ");

    press_in_body(cx, window, &secondary("z"));
    replayed.assert_matches(&browser, cx, "字下げの取り消し");
    press_in_body(cx, window, &secondary("z"));
    replayed.assert_matches(&browser, cx, "入力の取り消し");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- a\n- b",
            "取り消しで編集を始めたときの本文に戻らない"
        );
    });

    let redo = if cfg!(target_os = "macos") {
        "cmd-shift-z"
    } else {
        "ctrl-y"
    };
    press_in_body(cx, window, redo);
    replayed.assert_matches(&browser, cx, "入力のやり直し");
    press_in_body(cx, window, redo);
    replayed.assert_matches(&browser, cx, "字下げのやり直し");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "  - a\n  - bc",
            "やり直しで編集のあとの本文に戻らない"
        );
    });
}

#[gpui_kit::test]
fn 変換を確定した差分を当てると入力の本文になる(cx: &mut TestAppContext) {
    let (_dir, window, browser) = edit_body_at_end(cx, "前に");
    let replayed = ReplayedBody::subscribe(&browser, cx);

    cx.update_window(window.into(), |_, window, cx| {
        let mut input = body_input(&browser, window, cx);
        input.compose_text(None, "にほんご", Some(4..4), cx);
        input.compose_text(None, "日本語", Some(3..3), cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
    assert_eq!(replayed.text(), "前に", "変換中の文字の差分を出した");

    cx.update_window(window.into(), |_, window, cx| {
        body_input(&browser, window, cx).commit_text("日本語", cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
    replayed.assert_matches(&browser, cx, "変換の確定");
    let edits = replayed.edits.borrow().clone();
    assert_eq!(
        edits
            .last()
            .map(|edit| (edit.start_byte, edit.old_end_byte)),
        Some((0, "前に".len())),
        "変換の確定が、変換を始める前の本文の全体の置き換えになっていない"
    );
}

#[gpui_kit::test]
fn unmarkで確定した変換の差分は次の編集で全体の置き換えとして出る(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_body_at_end(cx, "前に");
    let replayed = ReplayedBody::subscribe(&browser, cx);

    cx.update_window(window.into(), |_, window, cx| {
        let mut input = body_input(&browser, window, cx);
        input.compose_text(None, "にほんご", Some(4..4), cx);
        input.compose_text(None, "日本語", Some(3..3), cx);
        input.unmark_text(cx);
    })
    .expect("ウィンドウを更新できなかった");
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "前に日本語",
            "確定した文字が入力に残っていない"
        );
    });
    assert_eq!(
        replayed.text(),
        "前に",
        "unmarkでの確定で差分が出るようになった (部品の挙動が変わった)"
    );

    commit_body(cx, window, &browser, "。");
    replayed.assert_matches(&browser, cx, "unmarkで確定したあとの入力");
    let edits = replayed.edits.borrow().clone();
    assert_eq!(
        edits
            .iter()
            .map(|edit| (edit.start_byte, edit.old_end_byte))
            .collect::<Vec<_>>(),
        [(0, "前に".len())],
        "確定した変換と次の入力が、変換を始める前の本文の全体の置き換えになっていない"
    );
}

#[gpui_kit::test]
fn 補完とリストの継続と字下げの差分を当てると入力の本文になる(
    cx: &mut TestAppContext,
) {
    let (_dir, window, browser) = edit_with_pages(cx, "- ");
    let replayed = ReplayedBody::subscribe(&browser, cx);

    commit_body_settled(cx, window, &browser, &["[", "[", "はじ"]);
    press_in_body(cx, window, "down");
    settle(cx, window);
    press_in_body(cx, window, "enter");
    replayed.assert_matches(&browser, cx, "補完の確定");

    press_in_body(cx, window, "enter");
    replayed.assert_matches(&browser, cx, "リストの継続");
    commit_body(cx, window, &browser, "次");
    let line_start = "- [[はじめに]]\n".len();
    cx.update(|cx| select_body(&browser, line_start..line_start + "- 次".len(), cx));
    press_in_body(cx, window, "tab");
    replayed.assert_matches(&browser, cx, "字下げの追加");
    press_in_body(cx, window, "shift-tab");
    replayed.assert_matches(&browser, cx, "字下げの削除");
    cx.update(|cx| {
        let end = body_text(&browser, cx).len();
        select_body(&browser, end..end, cx);
    });
    press_in_body(cx, window, "enter");
    press_in_body(cx, window, "enter");
    replayed.assert_matches(&browser, cx, "空の項目での記号の削除");
    cx.update(|cx| {
        assert_eq!(
            body_text(&browser, cx),
            "- [[はじめに]]\n- 次\n",
            "操作のあとの本文が期待と異なる"
        );
    });
}
