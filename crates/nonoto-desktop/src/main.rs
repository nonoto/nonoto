use std::path::PathBuf;
use std::process::ExitCode;

use gpui_kit::component::Theme;
use gpui_kit::{AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use nonoto_core::KeyStorage;
use nonoto_desktop::PageBrowser;

/// コマンド `nonoto` と同じ環境変数で、データの置き場と鍵の保管場所を変えられるようにする。
const DATA_DIR_ENV: &str = "NONOTO_DATA_DIR";
const KEY_STORE_ENV: &str = "NONOTO_KEY_STORE";

fn main() -> ExitCode {
    let pages = match open_pages() {
        Ok(pages) => pages,
        Err(message) => {
            eprintln!("エラー: {message}");
            return ExitCode::FAILURE;
        }
    };

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        nonoto_desktop::init(cx);
        let bounds = Bounds::centered(None, size(px(1024.), px(720.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Nonoto".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            // ライトとダークを、システムの外観に合わせて切り替える。
            Theme::sync_system_appearance(Some(window), cx);
            window
                .observe_window_appearance(|window, cx| {
                    Theme::sync_system_appearance(Some(window), cx);
                })
                .detach();
            cx.new(|cx| PageBrowser::new(pages, window, cx))
        });
        if let Err(err) = opened {
            eprintln!("エラー: ウィンドウを開けなかった: {err:#}");
            // macOSではアプリを終了するとプロセスが成功の終了コードで終わり、runから戻らない。
            // 失敗の終了コードで終わるよう、ここでプロセスを終える。
            // ストアは、ウィンドウの中身を作るクロージャーとともに、open_windowの中で既に閉じている。
            std::process::exit(1);
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
    ExitCode::SUCCESS
}

fn open_pages() -> Result<nonoto_core::Pages<nonoto_core::SqlCipherStore>, String> {
    let data_dir = match std::env::var_os(DATA_DIR_ENV) {
        Some(dir) => PathBuf::from(dir),
        None => nonoto_core::default_data_dir()
            .ok_or_else(|| format!("データの置き場が決まらない。{DATA_DIR_ENV} で指定する"))?,
    };
    let key_storage = match std::env::var(KEY_STORE_ENV).as_deref() {
        Ok("keychain") => KeyStorage::Keychain,
        Ok("file") => KeyStorage::File,
        Ok(other) => {
            return Err(format!(
                "{KEY_STORE_ENV} の値が正しくない (keychain か file を指定する): {other}"
            ));
        }
        Err(_) => KeyStorage::default(),
    };
    nonoto_core::open(&data_dir, key_storage).map_err(|err| nonoto_desktop::error_message(&err))
}
