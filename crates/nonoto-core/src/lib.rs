//! Nonotoのビジネスロジック。
//!
//! 各OSのクライアントとサーバーから共有するため、UIとCLIのクレートには依存しない。

mod author;
mod clock;
mod compaction;
mod crypto;
mod document;
mod editing;
mod envelope;
mod error;
mod generation;
mod key;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod keychain;
mod member;
mod page;
mod pages;
mod receive;
mod record;
mod store;
mod sync;
mod version;
mod wikilink;

use std::path::{Path, PathBuf};

pub use author::{Writer, WriterId, WriterNonce};
pub use clock::{Clock, SystemClock};
pub use compaction::{CheckedSnapshot, VersionRetention, check_snapshot};
pub use crypto::{
    Suite, TopicKey, UserSigningKey, UserVerifyingKey, WrapPublicKey, WrapSecretKey,
    WriteSigningKey, WriteVerifyingKey,
};
pub use document::{
    Affinity, Document, Replacement, Snapshot, Update, Version, VersionContent, map_position,
};
pub use editing::{TextEdit, continue_list, indent, outdent};
pub use envelope::Envelope;
pub use error::{Error, OpenStage};
pub use generation::{GenerationHeader, GenerationKeys, GenerationRecord, RoleKeys, WriteKeys};
pub use key::{FileKeyStore, Key, KeyStore};
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use keychain::KeychainKeyStore;
pub use member::{Member, MemberId, Membership, Role, UserKeys, UserPublicKeys};
pub use page::{NewPage, Page, PageId, PageSummary, PageUpdate};
pub use pages::Pages;
pub use receive::{
    CheckedEntry, CheckedUpdate, CheckedVersion, GenerationView, check_log_entry, check_log_record,
};
pub use record::{
    Branch, BranchId, RecordHeader, RecordKind, RecordPayload, SealedRecord, SyncPageId, TopicId,
    VerifiedRecord, WriteKeyRole,
};
pub use store::{PageStore, SqlCipherStore};
pub use sync::{
    CLIENT_VERSION, Incoming, MembershipChange, Outgoing, PageChange, RejectReason, RequestId,
    SnapshotInfo, Step, SuitePolicy, SyncClient,
};
pub use version::ContentHash;
pub use wikilink::{Wikilink, WikilinkQuery, complete_wikilink, find_wikilinks, wikilink_query};

/// データベースのファイル名。
const DATABASE_FILE: &str = "nonoto.db";

/// 鍵のファイル名。
const KEY_FILE: &str = "key";

/// 鍵の保管場所の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStorage {
    /// OSのキーチェーン。macOSとWindowsで使える。
    Keychain,
    /// データの置き場のファイル。OSのキーチェーンを使えない開発コンテナとテストで使う。
    File,
}

impl Default for KeyStorage {
    /// キーチェーンを使えるOSではキーチェーン、それ以外ではファイルにする。
    fn default() -> Self {
        if cfg!(any(target_os = "macos", target_os = "windows")) {
            Self::Keychain
        } else {
            Self::File
        }
    }
}

/// データの置き場にあるページを開く。
///
/// 置き場のディレクトリが無ければ作る。
/// 鍵は `key_storage` の保管場所から読み、無ければ作る。
/// 鍵の取得は、データベースのトランザクションの外 (開く前) で行う。
/// 日時はシステムの時計で決める。
pub fn open(data_dir: &Path, key_storage: KeyStorage) -> Result<Pages<SqlCipherStore>, Error> {
    open_with_clock(data_dir, key_storage, SystemClock)
}

/// データの置き場にあるページを、渡した時計で日時を決めるように開く。
///
/// 置き場と鍵の扱いは [`open`] と同じ。
pub fn open_with_clock(
    data_dir: &Path,
    key_storage: KeyStorage,
    clock: impl Clock + Send + 'static,
) -> Result<Pages<SqlCipherStore>, Error> {
    std::fs::create_dir_all(data_dir).map_err(Error::Io)?;
    let key = key_store(data_dir, key_storage)?.load_or_create()?;
    let store = SqlCipherStore::open(&data_dir.join(DATABASE_FILE), &key)?;
    Ok(Pages::with_clock(store, clock))
}

fn key_store(data_dir: &Path, key_storage: KeyStorage) -> Result<Box<dyn KeyStore>, Error> {
    match key_storage {
        KeyStorage::File => Ok(Box::new(FileKeyStore::new(data_dir.join(KEY_FILE)))),
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        KeyStorage::Keychain => Ok(Box::new(KeychainKeyStore::new(data_dir)?)),
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        KeyStorage::Keychain => Err(Error::UnsupportedKeyStorage("OSのキーチェーン")),
    }
}

/// OSの慣習に沿ったデータの置き場を返す。
///
/// OSがデータの置き場を持たない場合は `None` を返す。
pub fn default_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("nonoto"))
}
