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

    /// 鍵・ID・nonceを作るための乱数を得られない。
    #[error("鍵・ID・nonceを作るための乱数を得られない")]
    Random(#[source] getrandom::Error),

    /// ストアを開く処理が、鍵の違い以外の理由で失敗した。
    ///
    /// どの段階で失敗したかと、SQLiteの拡張エラーコードを残す。
    #[error(
        "ストアを開けない ({stage}で失敗した。SQLiteの拡張エラーコード: {})",
        code_text(*.extended_code)
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

    /// 文書に変更を書き込めない。
    #[error("文書に変更を書き込めない")]
    DocumentEdit,

    /// 受け取ったバイト列が、同期のレコードの識別の印で始まらない。
    #[error("同期のレコードの形式ではない")]
    UnknownRecordFormat,

    /// レコードの封筒の版を知らない。アプリの更新で読めるようになる。
    #[error("知らない封筒の版のレコードである: {0}")]
    UnsupportedEnvelopeVersion(u16),

    /// レコードの方式のIDを知らない。アプリの更新で読めるようになる。
    #[error("知らない方式のIDのレコードである: {0}")]
    UnsupportedSuite(u16),

    /// レコードの中身の版を知らない。アプリの更新で読めるようになる。
    #[error("知らない中身の版のレコードである: {0}")]
    UnsupportedContentVersion(u16),

    /// レコードが途中で切れているか、封筒や中身に知らない値を含む。
    #[error("レコードの形式が正しくない (途中で切れているか、知らない値を含む)")]
    MalformedRecord,

    /// レコードの中身が、封筒に書ける長さを超えている。
    #[error("レコードの中身が大きすぎる")]
    RecordTooLarge,

    /// レコードの書き込みの署名が、書き込みの公開鍵と合わない。
    #[error("レコードの書き込みの署名が合わない")]
    InvalidWriteSignature,

    /// 公開鍵のバイト列が、公開鍵として読めない。
    #[error("公開鍵の形式が正しくない")]
    InvalidPublicKey,

    /// レコードの中身を暗号化できない。
    #[error("レコードの中身を暗号化できない")]
    EncryptRecord,

    /// 同じIDのメンバーが、メンバー構成に2人いる。
    #[error("同じIDのメンバーがメンバー構成に2人いる")]
    DuplicateMember,

    /// 異なるメンバーが同じ利用者の公開鍵を持つ。
    #[error("異なるメンバーが同じ利用者の公開鍵を持っている")]
    DuplicateMemberKey,

    /// 指定したメンバーが、メンバー構成にいない。
    #[error("メンバー構成にいないメンバーである")]
    UnknownMember,

    /// 世代の記録に、自分宛ての包みが無い。
    #[error("世代の記録に自分宛ての包みが無い")]
    NotRecipient,

    /// 世代の記録を作ったメンバーが、編集の役割のメンバーではない。
    #[error("世代の記録を作ったメンバーが編集の役割ではない")]
    CreatorNotEditor,

    /// 世代の記録の作ったメンバーの署名が、そのメンバーの署名の公開鍵と合わない。
    #[error("世代の記録の作ったメンバーの署名が合わない")]
    InvalidCreatorSignature,

    /// 作成者の署名の鍵が、メンバー構成に登録された公開鍵と合わない。
    #[error("作成者の署名の鍵がメンバー構成の公開鍵と合わない")]
    CreatorKeyMismatch,

    /// 世代の記録の包みの中の役割か鍵が、受け手の役割か封筒の公開鍵と合わない。
    #[error("世代の記録の包みの中の鍵が、受け手の役割か封筒の公開鍵と合わない")]
    WrappedKeysMismatch,

    /// 文書に、書き手のIDを設定できない。
    #[error("文書に書き手のIDを設定できない")]
    InvalidWriterId,

    /// レコードの封筒の種類が、その処理で扱う種類ではない。
    #[error("レコードの封筒の種類が、この処理で扱う種類ではない")]
    UnexpectedRecordKind,

    /// レコードの世代の鍵を、確かめる側が持っていない。
    #[error("レコードの世代の鍵を持っていない: {0}")]
    UnknownGeneration(u64),

    /// 書き始めるメンバーの署名の公開鍵が、渡した利用者の鍵と合わない。
    #[error("書き始めるメンバーの署名の公開鍵が、利用者の鍵と合わない")]
    AuthorKeyMismatch,

    /// 更新の著者の署名が、著者のメンバーの利用者の公開鍵と合わない。
    #[error("更新の著者の署名が合わない")]
    InvalidAuthorSignature,

    /// 更新の書き手のIDが、著者の公開鍵と書き始めの乱数から導いた値と合わない。
    #[error("更新の書き手のIDが、著者の公開鍵と乱数から導いた値と合わない")]
    WriterIdMismatch,

    /// 更新が、著者の書き手のID以外の操作を含む。
    #[error("更新が、ほかの書き手の操作を含む")]
    ForeignOperations,

    /// 版の値が、版の値の形をしていない。
    #[error("版の値の形式が正しくない")]
    InvalidVersion,

    /// 文書が、版の時点までの操作を持っていない。
    #[error("文書が版の時点までの操作を持っていない")]
    VersionNotFound,

    /// 版の時点の中身のハッシュが、版の記録のハッシュと合わない。
    #[error("版の時点の中身が、版の記録の中身のハッシュと合わない")]
    VersionContentMismatch,

    /// 版の記録の、作ったメンバーの署名が合わない。
    #[error("版の記録の作ったメンバーの署名が合わない")]
    InvalidVersionSignature,

    /// ログのレコードの中身の種類が、その処理で扱う種類ではない。
    #[error("ログのレコードの中身の種類が、この処理で扱う種類ではない")]
    UnexpectedEntryKind,

    /// 書き直す先のレコードが、元のレコードと別のトピックかページを指している。
    #[error("書き直す先のレコードが、元のレコードと別のトピックかページを指している")]
    ResealTargetMismatch,

    /// 同期の処理で、開いていないページを指した。
    #[error("同期の処理で開いていないページである")]
    UnknownPage,

    /// 本文に書くのに、本文の書き込みの鍵を持っていない (編集の役割ではない)。
    #[error("本文の書き込みの鍵を持っていない")]
    NotEditor,

    /// レコードの中身を復号できない。鍵が違うか、封筒か暗号文が書き換えられている。
    #[error("レコードの中身を復号できない (鍵が違うか、書き換えられている)")]
    DecryptRecord,

    /// まとめたスナップショットの、まとめたメンバーの署名が合わない。
    #[error("スナップショットをまとめたメンバーの署名が合わない")]
    InvalidCompactorSignature,

    /// 版の中身を別に持つスナップショットは履歴を省いているため、文書を作れない。
    #[error("履歴を省いたスナップショットからは文書を作れない")]
    TrimmedSnapshot,

    /// ページをまとめる前提がそろっていない (つないで取得し終えていない・送っていない更新がある・まとめを送った結果を待っている)。
    #[error("ページをまとめる前提がそろっていない")]
    CompactionNotReady,

    /// 知らない封筒の版・方式・中身の版のレコードを受け取り、ページへの適用を止めている。
    #[error("知らない版か方式のレコードを受け取り、ページへの適用を止めている")]
    PageStalled,

    /// 中継が求める最低限のクライアントの版で、この端末が書ける方式が無い。
    #[error("書ける方式が無い")]
    NoWritableSuite,
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

fn code_text(code: Option<i32>) -> String {
    code.map_or_else(|| "無し".to_owned(), |code| code.to_string())
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Error::Database(err)
    }
}
