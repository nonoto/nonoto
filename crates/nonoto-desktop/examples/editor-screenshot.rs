//! 一覧の先頭のページの編集の画面を開いた状態で、デスクトップアプリを起動する。
//!
//! 開発コンテナの仮想ディスプレイで、編集の画面のスクリーンショットを取るために使う
//! (`scripts/screenshot-editor.sh`)。仮想ディスプレイには画面を操作する手段が無いため、起動したときに編集を始める。
//!
//! 使い方: `NONOTO_DATA_DIR=<データの置き場> editor-screenshot <light|dark>`
//! 鍵はデータの置き場のファイルから読む。

use std::process::ExitCode;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use nonoto_core::KeyStorage;
use nonoto_desktop::PageBrowser;

const DATA_DIR_ENV: &str = "NONOTO_DATA_DIR";

fn main() -> ExitCode {
    let mode = match std::env::args().nth(1).as_deref() {
        Some("light") => ThemeMode::Light,
        Some("dark") => ThemeMode::Dark,
        _ => {
            eprintln!("エラー: 外観を light か dark で指定する");
            return ExitCode::FAILURE;
        }
    };
    let Some(data_dir) = std::env::var_os(DATA_DIR_ENV) else {
        eprintln!("エラー: データの置き場を {DATA_DIR_ENV} で指定する");
        return ExitCode::FAILURE;
    };
    let pages = match nonoto_core::open(data_dir.as_ref(), KeyStorage::File) {
        Ok(pages) => pages,
        Err(err) => {
            eprintln!("エラー: {}", nonoto_desktop::error_message(&err));
            return ExitCode::FAILURE;
        }
    };

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        nonoto_desktop::init(cx);
        Theme::change(mode, None, cx);
        // 記法の見本の末尾まで撮影できる高さにする。仮想ディスプレイも同じ大きさにする。
        let bounds = Bounds::centered(None, size(px(1024.), px(1440.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                let mut browser = PageBrowser::new(pages, window, cx);
                browser.edit(window, cx);
                browser
            })
        })
        .expect("ウィンドウを開けなかった");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
    ExitCode::SUCCESS
}
