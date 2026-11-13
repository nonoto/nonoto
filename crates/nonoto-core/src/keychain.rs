//! 鍵をOSのキーチェーン (macOSのキーチェーン・Windowsの資格情報マネージャー) に保管する。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use keyring_core::{CredentialStore, Entry};

use crate::error::Error;
use crate::key::{Key, KeyStore, lock_key_store};

/// キーチェーンの項目のサービス名。
const SERVICE: &str = "nonoto";

/// 鍵をOSのキーチェーンに保管する。
///
/// 項目は、サービス名と、データの置き場ごとのアカウント名で分ける。
pub struct KeychainKeyStore {
    account: String,
    lock_path: PathBuf,
    store: Arc<CredentialStore>,
}

impl KeychainKeyStore {
    /// 既にあるデータの置き場に対応する項目と、初回作成を排他するロックを使う。
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn new(data_dir: &Path) -> Result<Self, Error> {
        Self::with_store(data_dir, native_store()?)
    }

    fn with_store(data_dir: &Path, store: Arc<CredentialStore>) -> Result<Self, Error> {
        // 相対パスやシンボリックリンクで指定しても、項目とロックの両方を同じにする。
        let data_dir = data_dir.canonicalize().map_err(Error::Io)?;
        Ok(Self {
            account: data_dir.to_string_lossy().into_owned(),
            lock_path: data_dir.join("keychain.lock"),
            store,
        })
    }

    fn entry(&self) -> Result<Entry, Error> {
        self.store
            .build(SERVICE, &self.account, None)
            .map_err(keychain_error)
    }
}

impl KeyStore for KeychainKeyStore {
    fn load_or_create(&self) -> Result<Key, Error> {
        let _lock = lock_key_store(&self.lock_path)?;
        let entry = self.entry()?;
        match entry.get_secret() {
            Ok(mut secret) => {
                let key = Key::from_bytes(&secret);
                zeroize::Zeroize::zeroize(&mut secret);
                key
            }
            Err(keyring_core::Error::NoEntry) => {
                let key = Key::generate()?;
                entry.set_secret(key.as_bytes()).map_err(keychain_error)?;
                Ok(key)
            }
            Err(err) => Err(keychain_error(err)),
        }
    }
}

#[cfg(target_os = "macos")]
fn native_store() -> Result<Arc<CredentialStore>, Error> {
    let store: Arc<CredentialStore> =
        apple_native_keyring_store::keychain::Store::new().map_err(keychain_error)?;
    Ok(store)
}

#[cfg(target_os = "windows")]
fn native_store() -> Result<Arc<CredentialStore>, Error> {
    let store: Arc<CredentialStore> =
        windows_native_keyring_store::Store::new().map_err(keychain_error)?;
    Ok(store)
}

fn keychain_error(err: keyring_core::Error) -> Error {
    Error::KeyStore(Box::new(err))
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;

    fn mock_store(data_dir: &Path) -> KeychainKeyStore {
        KeychainKeyStore::with_store(
            data_dir,
            keyring_core::mock::Store::new().expect("モックの保管場所を作れなかった"),
        )
        .expect("鍵の保管場所を作れなかった")
    }

    #[test]
    fn 同時に初めて取得しても全員が同じ鍵を得る() {
        for _ in 0..10 {
            let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
            let store = mock_store(dir.path());
            let barrier = Barrier::new(16);
            let keys = thread::scope(|scope| {
                let handles: Vec<_> = (0..16)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            store.load_or_create().expect("同時に鍵を取得できなかった")
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("鍵を取得するスレッドが失敗した"))
                    .collect::<Vec<_>>()
            });
            let saved = store
                .load_or_create()
                .expect("保存された鍵を取得できなかった");
            assert!(
                keys.iter().all(|key| key.as_bytes() == saved.as_bytes()),
                "同時に取得した鍵と保存された鍵が異なる"
            );
        }
    }

    #[test]
    fn 保存済みの鍵を上書きせず同じ鍵を返す() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let store = mock_store(dir.path());
        let key = Key::generate().expect("鍵を作れなかった");
        let entry = store.entry().expect("項目を作れなかった");
        entry
            .set_secret(key.as_bytes())
            .expect("鍵を保存できなかった");

        for _ in 0..2 {
            let loaded = store.load_or_create().expect("鍵を取得できなかった");
            assert!(
                loaded.as_bytes() == key.as_bytes(),
                "保存済みの鍵が変わった"
            );
        }
        assert!(
            entry.get_secret().expect("鍵を取得できなかった") == key.as_bytes(),
            "保管場所の鍵を上書きした"
        );
    }

    #[test]
    fn 不正な長さの鍵を上書きしない() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let store = mock_store(dir.path());
        let entry = store.entry().expect("項目を作れなかった");
        entry
            .set_secret(b"short")
            .expect("不正な鍵を保存できなかった");

        assert!(
            matches!(store.load_or_create(), Err(Error::InvalidKey)),
            "不正な長さの鍵を拒否しなかった"
        );
        assert_eq!(
            entry.get_secret().expect("保管場所を読めなかった"),
            b"short",
            "不正な鍵を作り直してしまった"
        );
    }

    #[test]
    fn 保管場所を読めなければ鍵を作らずエラーを返す() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let store = mock_store(dir.path());
        let entry = store.entry().expect("項目を作れなかった");
        let credential = entry
            .as_any()
            .downcast_ref::<keyring_core::mock::Cred>()
            .expect("モックの項目ではない");
        credential.set_error(keyring_core::Error::NoStorageAccess(Box::new(
            std::io::Error::other("検証用のアクセスエラー"),
        )));

        let result = store.load_or_create();

        assert!(
            matches!(result, Err(Error::KeyStore(_))),
            "読み取りエラーを返さなかった"
        );
        assert!(
            matches!(entry.get_secret(), Err(keyring_core::Error::NoEntry)),
            "読み取りに失敗したのに新しい鍵を保存した"
        );
        store
            .load_or_create()
            .expect("エラーのあとにロックが解放されなかった");
    }

    #[test]
    fn 新しい鍵を保存できなければエラーを返しロックを解放する() {
        use keyring_core::api::{CredentialApi, CredentialStoreApi};

        struct WriteFailure;
        impl CredentialApi for WriteFailure {
            fn get_secret(&self) -> keyring_core::Result<Vec<u8>> {
                Err(keyring_core::Error::NoEntry)
            }
            fn set_secret(&self, _: &[u8]) -> keyring_core::Result<()> {
                Err(keyring_core::Error::PlatformFailure(Box::new(
                    std::io::Error::other("検証用の保存エラー"),
                )))
            }
            fn delete_credential(&self) -> keyring_core::Result<()> {
                unreachable!()
            }
            fn get_credential(
                &self,
            ) -> keyring_core::Result<Option<Arc<keyring_core::Credential>>> {
                unreachable!()
            }
            fn get_specifiers(&self) -> Option<(String, String)> {
                None
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }
        impl CredentialStoreApi for WriteFailure {
            fn vendor(&self) -> String {
                "検証用".to_owned()
            }
            fn id(&self) -> String {
                "保存に失敗する保管場所".to_owned()
            }
            fn build(
                &self,
                _: &str,
                _: &str,
                _: Option<&std::collections::HashMap<&str, &str>>,
            ) -> keyring_core::Result<Entry> {
                Ok(Entry::new_with_credential(Arc::new(WriteFailure)))
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let store = KeychainKeyStore::with_store(dir.path(), Arc::new(WriteFailure))
            .expect("鍵の保管場所を作れなかった");
        let result = store.load_or_create();

        assert!(
            matches!(result, Err(Error::KeyStore(_))),
            "保存エラーを返さなかった"
        );
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&store.lock_path)
            .expect("ロック用のファイルを開けなかった");
        lock.try_lock()
            .expect("保存エラーのあとにロックが解放されなかった");
    }

    #[test]
    fn 同じ置き場の別表記でも同じ項目とロックを使う() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let provider = keyring_core::mock::Store::new().expect("モックを作れなかった");
        let first = KeychainKeyStore::with_store(dir.path(), provider.clone())
            .expect("鍵の保管場所を作れなかった");
        let second = KeychainKeyStore::with_store(&dir.path().join("."), provider)
            .expect("鍵の保管場所を作れなかった");
        let created = first.load_or_create().expect("鍵を作れなかった");
        let loaded = second.load_or_create().expect("鍵を取得できなかった");

        assert!(
            created.as_bytes() == loaded.as_bytes(),
            "別表記の置き場で鍵が異なる"
        );
        assert_eq!(
            first.lock_path, second.lock_path,
            "別表記の置き場でロックが異なる"
        );
    }
}
