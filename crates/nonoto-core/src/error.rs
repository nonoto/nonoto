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

    /// データベースの操作に失敗した。
    #[error("データベースの操作に失敗した")]
    Database(#[source] rusqlite::Error),

    /// データベースに保存した値が、ページの形をしていない。
    #[error("データベースに保存した値の形式が正しくない: {0}")]
    CorruptData(String),

    /// ファイルの読み書きに失敗した。
    #[error("ファイルの読み書きに失敗した")]
    Io(#[source] std::io::Error),
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Error::Database(err)
    }
}
