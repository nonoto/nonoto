use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use jiff::Timestamp;
use nonoto_core::{Key, Page, PageId, PageStore, SqlCipherStore};
use store_comparison::record::RecordStore;

fn run_worker(scheme: &str, dir: &Path, key: &Key) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_store-comparison"))
        .args(["--measure-open", scheme, "--dir"])
        .arg(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("計測用のプロセスを起動できなかった");
    child
        .stdin
        .take()
        .expect("標準入力を開けなかった")
        .write_all(key.as_bytes())
        .expect("計測用の鍵を渡せなかった");
    child
        .wait_with_output()
        .expect("計測用のプロセスを待てなかった")
}

#[test]
fn 計測用のプロセスは既存データを変更せず初回オープンの結果だけを返す() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let key = Key::generate().expect("鍵を作れなかった");
    let saved = Page {
        id: PageId::new(),
        title: "計測するページ".to_owned(),
        body: "東京都の会議".to_owned(),
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    };
    SqlCipherStore::open(&dir.path().join("a.db"), &key)
        .expect("方式Aを開けなかった")
        .insert(&saved)
        .expect("方式Aに保存できなかった");
    RecordStore::open(&dir.path().join("b.db"), &key)
        .expect("方式Bを開けなかった")
        .insert(&saved)
        .expect("方式Bに保存できなかった");

    for scheme in ["a", "b"] {
        let path = dir.path().join(format!("{scheme}.db"));
        let before = std::fs::read(&path).expect("データベースを読めなかった");

        let output = run_worker(scheme, dir.path(), &key);

        assert!(
            output.status.success(),
            "計測に失敗した: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("出力がUTF-8ではない");
        let fields: Vec<_> = stdout.split_whitespace().collect();
        assert_eq!(
            fields.len(),
            2,
            "初回オープン以外の計測やデータ投入を実行した: {stdout}"
        );
        assert!(fields[0].parse::<u64>().is_ok(), "起動時間が整数ではない");
        assert!(
            fields[1] == "-" || fields[1].parse::<u64>().is_ok(),
            "RSSの形式が違う"
        );
        assert_eq!(
            std::fs::read(&path).expect("データベースを読めなかった"),
            before,
            "計測用のプロセスがデータベースを変更した"
        );
    }
}

#[test]
fn 計測用のプロセスはデータベースを新しく作らない() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let key = Key::generate().expect("鍵を作れなかった");
    for scheme in ["a", "b"] {
        let output = run_worker(scheme, dir.path(), &key);
        assert!(
            !output.status.success(),
            "データベースが無いのに計測に成功した"
        );
    }
    assert_eq!(
        std::fs::read_dir(dir.path())
            .expect("ディレクトリを読めなかった")
            .count(),
        0,
        "計測用のプロセスがデータを作った"
    );
}
