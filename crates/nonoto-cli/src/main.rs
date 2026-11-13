mod output;

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use nonoto_core::{KeyStorage, NewPage, PageId, PageUpdate};

use crate::output::{PageOutput, PageSummaryOutput};

/// Nonotoのページを操作するコマンド。結果はJSONで標準出力に書く。
#[derive(Parser)]
#[command(name = "nonoto", version)]
struct Cli {
    /// データの置き場。省略するとOSの慣習に沿った置き場を使う
    #[arg(long, env = "NONOTO_DATA_DIR", global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// 鍵の保管場所。省略すると、キーチェーンを使えるOS (macOS・Windows) ではキーチェーン、それ以外ではファイルにする
    #[arg(long, env = "NONOTO_KEY_STORE", global = true, value_enum)]
    key_store: Option<KeyStoreArg>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum KeyStoreArg {
    /// OSのキーチェーン
    Keychain,
    /// データの置き場のファイル
    File,
}

impl From<KeyStoreArg> for KeyStorage {
    fn from(arg: KeyStoreArg) -> Self {
        match arg {
            KeyStoreArg::Keychain => KeyStorage::Keychain,
            KeyStoreArg::File => KeyStorage::File,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// ページを作成する
    Create {
        /// タイトル
        #[arg(long)]
        title: String,

        #[command(flatten)]
        body: BodyArgs,
    },
    /// ページを、更新した日時の新しい順に一覧する
    List,
    /// ページを取得する
    Get {
        /// ページのID
        id: PageId,
    },
    /// ページを更新する。指定しなかった項目は今の値のまま残す
    #[command(group(
        ArgGroup::new("changes")
            .required(true)
            .multiple(true)
            .args(["title", "body", "body_file"]),
    ))]
    Update {
        /// ページのID
        id: PageId,

        /// 新しいタイトル
        #[arg(long)]
        title: Option<String>,

        #[command(flatten)]
        body: BodyArgs,
    },
}

/// 本文の指定。シェルの引数に収まらない本文はファイルか標準入力から渡す。
#[derive(Args)]
#[group(multiple = false)]
struct BodyArgs {
    /// 本文 (Markdown)
    #[arg(long)]
    body: Option<String>,

    /// 本文 (Markdown) を読むファイル。`-` なら標準入力から読む
    #[arg(long, value_name = "PATH")]
    body_file: Option<PathBuf>,
}

impl BodyArgs {
    fn read(self) -> io::Result<Option<String>> {
        match (self.body, self.body_file) {
            (Some(body), _) => Ok(Some(body)),
            (None, Some(path)) if path.as_os_str() == "-" => {
                let mut body = String::new();
                io::stdin().read_to_string(&mut body)?;
                Ok(Some(body))
            }
            (None, Some(path)) => std::fs::read_to_string(path).map(Some),
            (None, None) => Ok(None),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("エラー: {}", error_chain(err.as_ref()));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = match cli.data_dir {
        Some(dir) => dir,
        None => nonoto_core::default_data_dir()
            .ok_or("データの置き場が決まらない。--data-dir か NONOTO_DATA_DIR で指定する")?,
    };
    let key_storage = cli.key_store.map(Into::into).unwrap_or_default();
    let mut pages = nonoto_core::open(&data_dir, key_storage)?;

    match cli.command {
        Command::Create { title, body } => {
            let body = body.read()?.unwrap_or_default();
            let page = pages.create(NewPage { title, body })?;
            print_json(&PageOutput::from(page))
        }
        Command::List => {
            let summaries: Vec<PageSummaryOutput> =
                pages.list()?.into_iter().map(Into::into).collect();
            print_json(&summaries)
        }
        Command::Get { id } => print_json(&PageOutput::from(pages.get(id)?)),
        Command::Update { id, title, body } => {
            let body = body.read()?;
            let page = pages.update(id, PageUpdate { title, body })?;
            print_json(&PageOutput::from(page))
        }
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(value)?;
    println!("{json}");
    Ok(())
}

/// エラーの原因をたどって、1行にまとめる。
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
