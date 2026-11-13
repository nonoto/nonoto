use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::Error;

/// 鍵の長さ (バイト)。
const KEY_LEN: usize = 32;

/// データベースを暗号化する鍵。
///
/// ログに出さないよう、`Debug` では中身を表示しない。
/// 使い終わったら、メモリ上の値を消す。
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Key([u8; KEY_LEN]);

impl Key {
    /// 乱数から新しい鍵を作る。
    pub fn generate() -> Result<Self, Error> {
        let mut bytes = [0u8; KEY_LEN];
        getrandom::fill(&mut bytes).map_err(Error::Random)?;
        Ok(Self(bytes))
    }

    /// 保管していたバイト列から鍵を作る。長さが違えば [`Error::InvalidKey`] を返す。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        <[u8; KEY_LEN]>::try_from(bytes)
            .map(Self)
            .map_err(|_| Error::InvalidKey)
    }

    /// 鍵を保管するためのバイト列を返す。
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(..)")
    }
}

/// 鍵の保管場所。
///
/// 端末ごとの保管場所 (OSのキーチェーン・ファイルなど) を、この裏に置く。
pub trait KeyStore {
    /// 鍵を読む。まだ無ければ、新しい鍵を作って保管する。
    ///
    /// 既にある鍵を作り直すと、その鍵で暗号化したデータを読めなくなるため、作るのは無いときだけにする。
    fn load_or_create(&self) -> Result<Key, Error>;
}

/// 鍵をファイルに保管する。
///
/// OSのキーチェーンを使えない開発コンテナとテストで使う。
/// ファイルは持ち主だけが読み書きできるようにする。
pub struct FileKeyStore {
    path: PathBuf,
}

impl FileKeyStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 新しい鍵を作って保存する。
    ///
    /// 呼び出し元がロックの中で鍵のファイルが無いことを確かめてから呼ぶ。
    /// 既にある鍵を上書きすると、その鍵で暗号化したデータを読めなくなるためである。
    fn create(&self) -> Result<Key, Error> {
        let key = Key::generate()?;
        // 書き込みの途中で止まっても中身の欠けた鍵のファイルを残さないよう、一時ファイルに書いて同期してから置き換える。
        let temp_path = self.path.with_added_extension("tmp");
        // 前回の作成の途中で残った一時ファイルは、権限を0600にして作り直すために消す。
        match std::fs::remove_file(&temp_path) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(Error::Io(err)),
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path).map_err(Error::Io)?;
        file.write_all(key.as_bytes()).map_err(Error::Io)?;
        file.sync_all().map_err(Error::Io)?;
        std::fs::rename(&temp_path, &self.path).map_err(Error::Io)?;
        Ok(key)
    }
}

impl KeyStore for FileKeyStore {
    fn load_or_create(&self) -> Result<Key, Error> {
        // 鍵の存在確認から書き込み・同期の完了まで、プロセス間で排他する。
        let _lock = lock_key_store(&self.path.with_added_extension("lock"))?;

        match File::open(&self.path) {
            Ok(file) => read_key(file),
            Err(err) if err.kind() == ErrorKind::NotFound => self.create(),
            Err(err) => Err(Error::Io(err)),
        }
    }
}

/// ファイルを閉じるまで、鍵の保管場所をプロセス間で排他する。
///
/// 全員が同じファイルをロックするよう、ロック用のファイルは削除しない。
pub(crate) fn lock_key_store(path: &Path) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(path).map_err(Error::Io)?;
    lock.lock().map_err(Error::Io)?;
    Ok(lock)
}

fn read_key(mut file: File) -> Result<Key, Error> {
    let mut bytes = Vec::with_capacity(KEY_LEN);
    file.read_to_end(&mut bytes).map_err(Error::Io)?;
    let result = Key::from_bytes(&bytes);
    bytes.zeroize();
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;

    #[test]
    fn 同時に初めて取得しても全員が同じ鍵を得る() {
        for _ in 0..10 {
            let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
            let store = FileKeyStore::new(dir.path().join("key"));
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

            let loaded = store.load_or_create().expect("保存された鍵を読めなかった");
            assert!(
                keys.iter().all(|key| key.as_bytes() == loaded.as_bytes()),
                "同時に取得した鍵と保存された鍵が異なる"
            );
        }
    }

    #[test]
    fn 鍵のファイルが無ければ作り_次からは同じ鍵を読む() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let store = FileKeyStore::new(dir.path().join("key"));

        let created = store.load_or_create().expect("鍵を作れなかった");
        let loaded = store.load_or_create().expect("鍵を読めなかった");

        assert_eq!(
            created.as_bytes(),
            loaded.as_bytes(),
            "2回目に読んだ鍵が作った鍵と異なる"
        );
    }

    #[test]
    fn 作成の途中で残った一時ファイルがあっても鍵を作れる() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("key");
        let temp_path = dir.path().join("key.tmp");
        std::fs::write(&temp_path, b"partial").expect("一時ファイルを書けなかった");
        let store = FileKeyStore::new(path.clone());

        let created = store.load_or_create().expect("鍵を作れなかった");
        let loaded = store.load_or_create().expect("鍵を読めなかった");

        assert_eq!(
            created.as_bytes(),
            loaded.as_bytes(),
            "2回目に読んだ鍵が作った鍵と異なる"
        );
        assert!(!temp_path.exists(), "一時ファイルが残っている");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = std::fs::metadata(&path)
                .expect("鍵のファイルの情報を読めなかった")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "鍵のファイルの権限が0600ではない");
        }
    }

    #[cfg(unix)]
    #[test]
    fn 鍵のファイルは持ち主だけが読み書きできる() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("key");
        FileKeyStore::new(path.clone())
            .load_or_create()
            .expect("鍵を作れなかった");

        let mode = std::fs::metadata(&path)
            .expect("鍵のファイルの情報を読めなかった")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "鍵のファイルの権限が0600ではない");
    }

    #[test]
    fn 鍵の長さが違うファイルは読まない() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let path = dir.path().join("key");
        std::fs::write(&path, b"short").expect("ファイルを書けなかった");

        let err = FileKeyStore::new(path)
            .load_or_create()
            .expect_err("形式の違う鍵のファイルを読めてしまった");

        assert!(
            matches!(err, Error::InvalidKey),
            "エラーが鍵の形式のエラーではない: {err:?}"
        );
    }

    #[test]
    fn 鍵をデバッグ表示しても中身を出さない() {
        let key = Key::generate().expect("鍵を作れなかった");

        assert_eq!(
            format!("{key:?}"),
            "Key(..)",
            "デバッグ表示に鍵の中身が出た"
        );
    }
}
