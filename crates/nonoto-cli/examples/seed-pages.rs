//! 補完の検索の性能を確かめるため、短いページをまとめて作る開発用のコマンド。

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use nonoto_core::{KeyStorage, NewPage};

#[derive(Parser)]
struct Args {
    /// テスト用のデータの置き場。普段のデータと分けて明示する
    #[arg(long, env = "NONOTO_DATA_DIR")]
    data_dir: PathBuf,

    /// 鍵の保管場所。省略時はnonotoコマンドと同じ既定値を使う
    #[arg(long, env = "NONOTO_KEY_STORE", value_enum)]
    key_store: Option<KeyStoreArg>,

    /// 追加するページの件数
    #[arg(long, default_value = "10000")]
    count: NonZeroUsize,
}

#[derive(Clone, Copy, ValueEnum)]
enum KeyStoreArg {
    Keychain,
    File,
}

fn main() -> ExitCode {
    match seed(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("エラー: {error}");
            ExitCode::FAILURE
        }
    }
}

fn seed(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let key_storage = match args.key_store {
        Some(KeyStoreArg::Keychain) => KeyStorage::Keychain,
        Some(KeyStoreArg::File) => KeyStorage::File,
        None => KeyStorage::default(),
    };
    // 鍵の取得とデータベースを開く処理は一度だけにし、既存のPages APIで作る。
    let mut pages = nonoto_core::open(&args.data_dir, key_storage)?;
    for number in 1..=args.count.get() {
        pages.create(NewPage {
            title: format!("検証用ページ {number:05}"),
            body: "補完の検索を確かめるための短い本文。".to_owned(),
        })?;
    }
    println!("ページを{}件作った", args.count);
    Ok(())
}
