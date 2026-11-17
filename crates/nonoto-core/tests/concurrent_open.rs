//! プロセスの中で初めてストアを開く処理を、多数のスレッドから同時に行う再現のテスト。
//!
//! SQLiteとSQLCipherの初期化はプロセスに1回だけ走るため、再現のテストでは、プロセスの中で最初にストアを開く。
//! 1つのプロセスでは1つのテストだけを動かす (`--exact` で名前を指定する)。
//! 再現のテストには `#[ignore]` を付け、プロセスごとに繰り返して動かす。
//! 毎回の `make test` では、回帰テストがこのテストのバイナリを子のプロセスとして繰り返し起動する。
//! 回数を増やして頻度を見るときは、`make repeat-test` を使う。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Barrier;
use std::thread;

use nonoto_core::{Key, SqlCipherStore};

/// 同時に開くスレッドの数。`NONOTO_OPEN_THREADS` で変えられる。
fn threads() -> usize {
    match std::env::var("NONOTO_OPEN_THREADS") {
        Ok(value) => value
            .parse()
            .ok()
            .filter(|threads| *threads > 0)
            .expect("NONOTO_OPEN_THREADSは正の整数で指定する"),
        Err(std::env::VarError::NotPresent) => 16,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("NONOTO_OPEN_THREADSは正の整数で指定する")
        }
    }
}

/// `paths` のそれぞれを、別々のスレッドから同時に開く。失敗したスレッドのエラーの表示を返す。
fn open_concurrently(paths: &[PathBuf], key: &Key) -> Vec<String> {
    let barrier = Barrier::new(paths.len());
    thread::scope(|scope| {
        let handles: Vec<_> = paths
            .iter()
            .map(|path| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    SqlCipherStore::open(path, key).err()
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().expect("ストアを開くスレッドが失敗した"))
            .map(|err| format!("{err} / {err:?}"))
            .collect()
    })
}

/// 鍵を、鍵を渡す文に入れる小文字の16進数にする。
fn key_hex(key: &Key) -> String {
    key.as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 鍵の16進数が、エラーの表示に含まれていないことを確かめる。
fn assert_no_key(errors: &[String], key: &Key) {
    let hex = key_hex(key);
    for err in errors {
        assert!(
            !err.to_lowercase().contains(&hex),
            "エラーの表示に鍵が含まれている"
        );
    }
}

fn check(paths: &[PathBuf], key: &Key) {
    let errors = open_concurrently(paths, key);
    assert_no_key(&errors, key);
    assert!(
        errors.is_empty(),
        "{}個のスレッドのうち{}個でストアを開けなかった:\n{}",
        paths.len(),
        errors.len(),
        errors.join("\n")
    );
}

#[test]
#[ignore = "プロセスの初期化の競合を見るため、make repeat-test でプロセスごと繰り返す"]
fn プロセスで初めて別々のファイルのストアを同時に開ける() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let key = Key::generate().expect("鍵を作れなかった");
    let paths: Vec<PathBuf> = (0..threads())
        .map(|i| dir.path().join(format!("nonoto-{i}.db")))
        .collect();

    check(&paths, &key);
}

/// 回帰テストが子のプロセスを起動する回数。`NONOTO_OPEN_PROCESSES` で変えられる。
fn processes() -> usize {
    match std::env::var("NONOTO_OPEN_PROCESSES") {
        Ok(value) => value
            .parse()
            .ok()
            .filter(|processes| *processes > 0)
            .expect("NONOTO_OPEN_PROCESSESは正の整数で指定する"),
        Err(std::env::VarError::NotPresent) => 60,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("NONOTO_OPEN_PROCESSESは正の整数で指定する")
        }
    }
}

/// 回帰テストの子のプロセスが同時に開くスレッドの数。スレッドが多いほど、初期化の競合が起きやすい。
const REGRESSION_THREADS: &str = "64";

/// 再現のテストを、このテストのバイナリの子のプロセスとして1回動かす。
fn run_child(test: &str) -> Output {
    let exe = std::env::current_exe().expect("テストのバイナリのパスを得られなかった");
    Command::new(exe)
        .args(["--ignored", "--exact", test, "--quiet"])
        .env("NONOTO_OPEN_THREADS", REGRESSION_THREADS)
        .output()
        .expect("子のプロセスを起動できなかった")
}

/// 子のプロセスが、再現のテストを1件動かして成功したか。
///
/// 名前が一致しないと、子は1件も動かさずに成功で終わるため、終了コードに加えて件数を確かめる。
fn child_passed(output: &Output) -> bool {
    output.status.success()
        && String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;")
}

#[test]
fn 別々のプロセスで初めてストアを同時に開いても失敗しない() {
    const TEST: &str = "プロセスで初めて別々のファイルのストアを同時に開ける";
    let runs = processes();
    let parallel = thread::available_parallelism().map_or(1, |n| n.get());
    let mut failures = Vec::new();
    for start in (0..runs).step_by(parallel) {
        let outputs: Vec<Output> = thread::scope(|scope| {
            let handles: Vec<_> = (start..runs.min(start + parallel))
                .map(|_| scope.spawn(|| run_child(TEST)))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("子のプロセスを待つスレッドが失敗した"))
                .collect()
        });
        failures.extend(
            outputs
                .into_iter()
                .filter(|output| !child_passed(output))
                .map(|output| {
                    format!(
                        "{}\n{}{}",
                        output.status,
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    )
                }),
        );
    }
    assert!(
        failures.is_empty(),
        "{runs}回のうち{}回で、子のプロセスの再現のテストが成功しなかった:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
#[ignore = "プロセスの初期化の競合を見るため、make repeat-test でプロセスごと繰り返す"]
fn プロセスで初めて同じファイルのストアを同時に開ける() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let key = Key::generate().expect("鍵を作れなかった");
    let path = dir.path().join("nonoto.db");
    let paths = vec![path; threads()];

    check(&paths, &key);
}

/// SQLCipherのログの水準。`NONOTO_CIPHER_LOG_LEVEL` で変えられる (既定はERROR)。
///
/// DEBUGにすると経路を細かく追えるが、ログが増えて1回の実行が遅くなる。
/// SQLCipherの `PRAGMA cipher_log_level` と同じく大文字と小文字を区別せず、どの水準にも当たらない値は拒否する。
fn cipher_log_level() -> &'static str {
    const USAGE: &str =
        "NONOTO_CIPHER_LOG_LEVELはERROR・WARN・INFO・DEBUG・TRACEのいずれかで指定する";
    let value = match std::env::var("NONOTO_CIPHER_LOG_LEVEL") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return "ERROR",
        Err(std::env::VarError::NotUnicode(_)) => panic!("{USAGE}"),
    };
    match value.to_ascii_uppercase().as_str() {
        "ERROR" => "ERROR",
        "WARN" => "WARN",
        "INFO" => "INFO",
        "DEBUG" => "DEBUG",
        "TRACE" => "TRACE",
        _ => panic!("{USAGE} (指定した値: {value})"),
    }
}

/// プロセスの初期化を先に済ませずに、ファイルへの接続と鍵を渡すまでを行う。`log` があれば、SQLCipherのログをそこに書かせる。
///
/// 製品のコードはSQLCipherのログを止め、初期化も先に済ませるため、初期化の競合をログで見るときはこちらを使う。
/// SQLCipherのログの書き先はプロセスで1つで、設定し直すと前のファイルを排他無しで閉じる。
/// 複数のスレッドから設定すると同じファイルを二重に閉じてしまうため、`log` は1つのスレッドにだけ渡す。
fn open_with_cipher_log(path: &Path, key: &Key, log: Option<&Path>) -> Result<(), rusqlite::Error> {
    let conn = rusqlite::Connection::open(path)?;
    if let Some(log) = log {
        let log = log.to_str().expect("ログのパスがUTF-8ではない");
        set_cipher_log(&conn, log)?;
        conn.execute_batch(&format!(
            "PRAGMA cipher_log_level = {};",
            cipher_log_level()
        ))?;
    }
    conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", key_hex(key)))?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
}

/// ログの書き先を設定する。SQLCipherは失敗をSQLのエラーではなく結果の行に返すため、その値も読む。
fn set_cipher_log(conn: &rusqlite::Connection, destination: &str) -> rusqlite::Result<()> {
    let status: String = conn.query_row(
        &format!("PRAGMA cipher_log = '{}';", destination.replace('\'', "''")),
        [],
        |row| row.get(0),
    )?;
    if status != "0" {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "SQLCipherのログの書き先を設定できない (書き先: {destination}、結果: {status})"
            )),
        ));
    }
    Ok(())
}

/// ログのファイルの今の長さ。ファイルが無ければ0。
///
/// SQLCipherはログのファイルに追記するため、書き先を設定する前に控えた長さより後ろを、その実行で書いた部分として読む。
fn cipher_log_len(log: &Path) -> u64 {
    match std::fs::metadata(log) {
        Ok(metadata) => metadata.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
        Err(err) => panic!(
            "SQLCipherのログの長さを読めなかった ({}): {err}",
            log.display()
        ),
    }
}

/// スレッドがすべて終わってから呼ぶ。ログを閉じてCのバッファを出力し、`start` バイト目より後ろを読む。
fn close_and_read_cipher_log(log: &Path, start: u64) -> Result<String, Box<dyn std::error::Error>> {
    let conn = rusqlite::Connection::open_in_memory()?;
    set_cipher_log(&conn, "off")?;
    let mut file = std::fs::File::open(log)?;
    file.seek(SeekFrom::Start(start))?;
    let mut written = String::new();
    file.read_to_string(&mut written)?;
    Ok(written)
}

#[test]
fn sqlcipherのログの設定と読み取りを確かめる() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let log = dir.path().join("sqlcipher.log");
    // 保存先を使い回したときに、前の実行が残した行。
    std::fs::write(&log, "前の実行の行\n").expect("ログのファイルを作れなかった");
    let start = cipher_log_len(&log);
    let conn = rusqlite::Connection::open_in_memory().expect("接続を開けなかった");
    conn.execute_batch("PRAGMA cipher_log_level = INFO;")
        .expect("ログの水準を設定できなかった");
    // 書き先の設定が出す短いINFOのログは、閉じるまでCのバッファに残る。
    set_cipher_log(&conn, log.to_str().expect("ログのパスがUTF-8ではない"))
        .expect("ログの書き先を設定できなかった");

    let written = close_and_read_cipher_log(&log, start).expect("ログを閉じて読めなかった");
    assert!(
        written.contains("sqlcipher_set_log: set log to"),
        "ログを閉じて読んだ内容に、短いINFOのログが残っていない"
    );
    assert!(
        !written.contains("前の実行の行"),
        "ログを読んだ内容に、前の実行が残した行が混ざっている"
    );

    let missing = dir.path().join("無いディレクトリ").join("sqlcipher.log");
    assert_eq!(cipher_log_len(&missing), 0, "無いログの長さが0ではない");
    assert!(
        set_cipher_log(&conn, missing.to_str().expect("ログのパスがUTF-8ではない")).is_err(),
        "書けない保存先の設定が成功した"
    );
    assert!(
        close_and_read_cipher_log(&missing, 0).is_err(),
        "存在しないログの読み取りが成功した"
    );
}

#[test]
#[ignore = "プロセスの初期化の競合を見るため、make repeat-test でプロセスごと繰り返す"]
fn プロセスで初めて鍵を渡すときのsqlcipherのログを取る() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let key = Key::generate().expect("鍵を作れなかった");
    let log = std::env::var_os("NONOTO_CIPHER_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.path().join("sqlcipher.log"));
    let start = cipher_log_len(&log);
    let barrier = Barrier::new(threads());

    let errors: Vec<String> = thread::scope(|scope| {
        let handles: Vec<_> = (0..threads())
            .map(|i| {
                let (barrier, key, log) = (&barrier, &key, &log);
                let path = dir.path().join(format!("nonoto-{i}.db"));
                let log = (i == 0).then_some(log.as_path());
                scope.spawn(move || {
                    barrier.wait();
                    open_with_cipher_log(&path, key, log).err()
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().expect("ストアを開くスレッドが失敗した"))
            .map(|err| {
                // rusqliteのエラーの表示は、失敗した文を含む。鍵を出さないよう、コードとメッセージだけを残す。
                match err.sqlite_error() {
                    Some(code) => {
                        format!("拡張エラーコード{}: {}", code.extended_code, message(&err))
                    }
                    None => "SQLiteのエラーではない".to_owned(),
                }
            })
            .collect()
    });

    let written = close_and_read_cipher_log(&log, start)
        .unwrap_or_else(|err| panic!("SQLCipherのログを読めなかった ({}): {err}", log.display()));
    assert!(
        !written.to_lowercase().contains(&key_hex(&key)),
        "SQLCipherのログに鍵が含まれている"
    );
    assert!(
        errors.is_empty(),
        "{}個のスレッドで鍵を渡せなかった:\n{}\nSQLCipherのログのうちこの実行で書いた部分 ({}。書き先を設定する前のログは標準エラーに出る):\n{}",
        errors.len(),
        errors.join("\n"),
        log.display(),
        written
            .lines()
            .filter(|line| line.contains("ERROR") || line.contains("WARN"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn message(err: &rusqlite::Error) -> String {
    match err {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.clone(),
        rusqlite::Error::SqlInputError { msg, .. } => msg.clone(),
        _ => String::new(),
    }
}
