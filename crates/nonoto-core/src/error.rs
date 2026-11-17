use crate::page::PageId;

/// コアの操作で起きるエラー。
///
/// 鍵とページの本文は、エラーメッセージに含めない。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 指定したIDのページが無い。
    #[error("ページが見つからない: {0}")]
    PageNotFound(PageId),

    /// データベースを鍵で開けない。鍵が違うか、ファイルが壊れている。
    #[error("データベースを鍵で開けない (鍵が違うか、ファイルが壊れている)")]
    WrongKey,

    /// 保管していた鍵が、鍵の形をしていない。
    #[error("保管していた鍵の形式が正しくない")]
    InvalidKey,

    /// 鍵の保管場所 (OSのキーチェーンなど) を読み書きできない。
    #[error("鍵の保管場所を読み書きできない")]
    KeyStore(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// このOSでは、指定した鍵の保管場所を使えない。
    #[error("このOSでは、鍵の保管場所に{0}を使えない")]
    UnsupportedKeyStorage(&'static str),

    /// 鍵を作るための乱数を得られない。
    #[error("鍵を作るための乱数を得られない: {0}")]
    Random(getrandom::Error),

    /// ストアを開く処理が、鍵の違い以外の理由で失敗した。
    ///
    /// どの段階で失敗したかと、SQLiteの拡張エラーコードを残す。
    #[error(
        "ストアを開けない ({stage}で失敗した。SQLiteの拡張エラーコード: {})",
        code_text(.extended_code)
    )]
    OpenStore {
        /// 失敗した段階。
        stage: OpenStage,
        /// SQLiteの拡張エラーコード。SQLiteのエラーでなければ `None`。
        extended_code: Option<i32>,
        /// 失敗の原因。鍵を渡す段階では、鍵を含む文を取り除いてある。
        #[source]
        source: rusqlite::Error,
    },

    /// データベースの操作に失敗した。
    #[error("データベースの操作に失敗した")]
    Database(#[source] rusqlite::Error),

    /// データベースに保存した値が、ページの形をしていない。
    #[error("データベースに保存した値の形式が正しくない: {0}")]
    CorruptData(String),

    /// ファイルの読み書きに失敗した。
    #[error("ファイルの読み書きに失敗した")]
    Io(#[source] std::io::Error),

    /// 文書の編集の範囲が、本文の外にあるか、文字の途中を指している。
    #[error("編集の範囲が本文の外にあるか、文字の途中を指している")]
    InvalidRange,

    /// 届いた更新を文書に当てられない。壊れているか、別の形式である。
    #[error("更新を文書に当てられない (壊れているか、別の形式である)")]
    InvalidUpdate,

    /// スナップショットから文書を読み込めない。壊れているか、別の形式である。
    #[error("スナップショットから文書を読み込めない (壊れているか、別の形式である)")]
    InvalidSnapshot,

    /// 文書の更新やスナップショットを書き出せない。
    #[error("文書を書き出せない")]
    DocumentExport,
}

/// ストアを開く処理の段階。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenStage {
    /// SQLiteとSQLCipherのプロセスの初期化を済ませる。
    Initialize,
    /// データベースのファイルに接続し、SQLCipherのログを止める。
    Connect,
    /// SQLCipherに鍵を渡す (`PRAGMA key`)。
    ApplyKey,
    /// 鍵でデータベースを読めるかを確かめる。
    VerifyKey,
    /// スキーマを初期化する。
    Migrate,
}

impl std::fmt::Display for OpenStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Initialize => "SQLiteとSQLCipherの初期化",
            Self::Connect => "接続",
            Self::ApplyKey => "鍵の設定",
            Self::VerifyKey => "鍵の確認",
            Self::Migrate => "スキーマの初期化",
        })
    }
}

fn code_text(code: &Option<i32>) -> String {
    code.map_or_else(|| "無し".to_owned(), |code| code.to_string())
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Error::Database(err)
    }
}
