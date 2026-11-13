//! データ投入で使ったヒープを引き継がない、別プロセスでの初回オープンの計測。

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use nonoto_core::{Key, SqlCipherStore};
use store_comparison::record::RecordStore;
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
pub enum Scheme {
    A,
    B,
}

impl Scheme {
    pub fn parse(value: &str) -> Self {
        match value {
            "a" => Self::A,
            "b" => Self::B,
            _ => panic!("計測する方式はaかbで指定する"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

pub struct Measurement {
    pub elapsed: Duration,
    pub rss_increase_kib: Option<u64>,
}

/// 既存のデータベースを初めて開く。鍵は親から標準入力のパイプで受け取る。
pub fn run_worker(scheme: Scheme, dir: &Path) {
    let path = dir.join(format!("{}.db", scheme.name()));
    let mut bytes = Zeroizing::new([0u8; 32]);
    std::io::stdin()
        .read_exact(bytes.as_mut())
        .expect("計測用の鍵を受け取れなかった");
    let key = Key::from_bytes(bytes.as_ref()).expect("計測用の鍵の長さが違う");
    assert!(path.is_file(), "計測するデータベースが無い");
    let measured = match scheme {
        Scheme::A => measure(|| SqlCipherStore::open(&path, &key).expect("方式Aを開けなかった")),
        Scheme::B => measure(|| RecordStore::open(&path, &key).expect("方式Bを開けなかった")),
    };
    let rss = measured
        .rss_increase_kib
        .map(|kib| kib.to_string())
        .unwrap_or_else(|| "-".to_owned());
    println!("{} {rss}", measured.elapsed.as_nanos());
}

/// ストアを保持したままRSSを読み、解放の時間は起動時間に含めない。
fn measure<T>(open: impl FnOnce() -> T) -> Measurement {
    let before = resident_kib();
    let (store, elapsed) = crate::timed(open);
    let after = resident_kib();
    std::hint::black_box(&store);
    Measurement {
        elapsed,
        rss_increase_kib: before
            .zip(after)
            .map(|(before, after)| after.saturating_sub(before)),
    }
}

pub fn in_new_process(scheme: Scheme, dir: &Path, key: &Key) -> Measurement {
    let mut child =
        Command::new(std::env::current_exe().expect("実行ファイルの場所を取得できなかった"))
            .args(["--measure-open", scheme.name(), "--dir"])
            .arg(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("計測用のプロセスを起動できなかった");
    child
        .stdin
        .take()
        .expect("鍵を渡すパイプを開けなかった")
        .write_all(key.as_bytes())
        .expect("計測用の鍵を渡せなかった");
    let output = child
        .wait_with_output()
        .expect("計測用のプロセスを待てなかった");
    assert!(output.status.success(), "計測用のプロセスが失敗した");
    let output = String::from_utf8(output.stdout).expect("計測結果がUTF-8ではない");
    let (nanos, rss) = output.trim().split_once(' ').expect("計測結果の形式が違う");
    Measurement {
        elapsed: Duration::from_nanos(nanos.parse().expect("計測時間が整数ではない")),
        rss_increase_kib: if rss == "-" {
            None
        } else {
            Some(rss.parse().expect("RSSが整数ではない"))
        },
    }
}

/// Linuxでは、このプロセスの常駐メモリ (KiB) を返す。
fn resident_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}
