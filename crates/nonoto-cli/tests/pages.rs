use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Barrier;
use std::thread;

use serde_json::Value;

/// データの置き場を `--data-dir` で指定してコマンドを実行する。
fn run(data_dir: &Path, args: &[&str]) -> Output {
    run_with_stdin(data_dir, args, "")
}

fn run_with_stdin(data_dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nonoto"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .env_remove("NONOTO_DATA_DIR")
        // テストで開発者のキーチェーンに項目を作らないよう、鍵はデータの置き場のファイルに置く。
        .env("NONOTO_KEY_STORE", "file")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("コマンド nonoto を実行できなかった");
    child
        .stdin
        .take()
        .expect("標準入力を開けなかった")
        .write_all(stdin.as_bytes())
        .expect("標準入力に書けなかった");
    child
        .wait_with_output()
        .expect("コマンド nonoto の終了を待てなかった")
}

/// 成功したことを確かめ、標準出力のJSONを返す。
fn json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "終了コードが成功ではない: {:?}, 標準エラー: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("標準出力がJSONではない")
}

fn create(data_dir: &Path, title: &str, body: &str) -> Value {
    json(run(data_dir, &["create", "--title", title, "--body", body]))
}

#[test]
fn 初めての置き場で複数のプロセスから同時にページを作成できる() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let barrier = Barrier::new(8);
    let created = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let data_dir = dir.path();
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    create(data_dir, &format!("ページ{index}"), "本文")
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("ページを作成するスレッドが失敗した"))
            .collect::<Vec<_>>()
    });

    let list = json(run(dir.path(), &["list"]));
    assert_eq!(
        list.as_array().expect("一覧が配列ではない").len(),
        created.len(),
        "同時に作成したページの件数が異なる"
    );
    for page in created {
        let id = page["id"].as_str().expect("IDが文字列ではない");
        assert_eq!(
            json(run(dir.path(), &["get", id])),
            page,
            "同時に作成したページを別のプロセスから読めない"
        );
    }
}

#[test]
fn 作成したページを取得できる() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");

    let created = create(dir.path(), "はじめに", "# 見出し\n\n本文");
    let id = created["id"].as_str().expect("IDが文字列ではない");
    let got = json(run(dir.path(), &["get", id]));

    assert_eq!(got, created, "取得したページが作成したページと異なる");
    assert_eq!(got["title"], "はじめに", "タイトルが期待と異なる");
    assert_eq!(got["body"], "# 見出し\n\n本文", "本文が期待と異なる");
}

#[test]
fn 一覧は本文を含めず_更新した日時の新しい順に並ぶ() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let first = create(dir.path(), "1つめ", "本文");
    create(dir.path(), "2つめ", "本文");
    let first_id = first["id"].as_str().expect("IDが文字列ではない");
    json(run(
        dir.path(),
        &["update", first_id, "--title", "1つめ (更新)"],
    ));

    let list = json(run(dir.path(), &["list"]));

    let items = list.as_array().expect("一覧が配列ではない");
    let titles: Vec<&str> = items
        .iter()
        .map(|item| item["title"].as_str().expect("タイトルが文字列ではない"))
        .collect();
    assert_eq!(
        titles,
        ["1つめ (更新)", "2つめ"],
        "一覧の並びが期待と異なる"
    );
    assert!(
        items.iter().all(|item| item.get("body").is_none()),
        "一覧に本文が含まれている"
    );
}

#[test]
fn 更新で指定しなかった項目は残る() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let created = create(dir.path(), "タイトル", "本文");
    let id = created["id"].as_str().expect("IDが文字列ではない");

    let updated = json(run_with_stdin(
        dir.path(),
        &["update", id, "--body-file", "-"],
        "標準入力の\n本文\n",
    ));

    assert_eq!(
        updated["title"], "タイトル",
        "指定しなかったタイトルが変わった"
    );
    assert_eq!(
        updated["body"], "標準入力の\n本文\n",
        "本文が標準入力の内容ではない"
    );
    assert_eq!(
        updated["created_at"], created["created_at"],
        "作成日時が変わった"
    );
}

#[test]
fn 本文をファイルから読んで作成できる() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let body_dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let body_path = body_dir.path().join("body.md");
    std::fs::write(&body_path, "# ファイルの\n\n本文\n").expect("本文のファイルを書けなかった");

    let created = json(run(
        dir.path(),
        &[
            "create",
            "--title",
            "タイトル",
            "--body-file",
            body_path
                .to_str()
                .expect("本文のファイルのパスがUTF-8ではない"),
        ],
    ));

    assert_eq!(
        created["body"], "# ファイルの\n\n本文\n",
        "本文がファイルの内容ではない"
    );
}

#[test]
fn 本文とファイルを同時に指定すると引数のエラーになる() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");

    let output = run(
        dir.path(),
        &[
            "create",
            "--title",
            "タイトル",
            "--body",
            "本文",
            "--body-file",
            "-",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(2),
        "引数のエラーの終了コード2ではない"
    );
}

#[test]
fn 更新する項目が無いと失敗する() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let created = create(dir.path(), "タイトル", "本文");
    let id = created["id"].as_str().expect("IDが文字列ではない");

    let output = run(dir.path(), &["update", id]);

    assert!(!output.status.success(), "更新する項目が無いのに成功した");
}

#[test]
fn 無いページを取得すると失敗する() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let id = "01a1d137-4c48-7275-8597-6d95b5a76a11";

    let output = run(dir.path(), &["get", id]);

    assert_eq!(output.status.code(), Some(1), "終了コードが1ではない");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("ページが見つからない: {id}")),
        "標準エラーにページが見つからない旨が無い: {stderr}"
    );
}

#[test]
fn 環境変数でデータの置き場を指定できる() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let created = create(dir.path(), "タイトル", "本文");
    let id = created["id"].as_str().expect("IDが文字列ではない");

    let output = Command::new(env!("CARGO_BIN_EXE_nonoto"))
        .args(["get", id])
        .env("NONOTO_DATA_DIR", dir.path())
        .env("NONOTO_KEY_STORE", "file")
        .output()
        .expect("コマンド nonoto を実行できなかった");

    assert_eq!(json(output), created, "環境変数の置き場のページを読めない");
}

#[test]
fn データの置き場ごとに別のページを持つ() {
    let first = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let second = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    create(first.path(), "1つめの置き場のページ", "本文");

    let list = json(run(second.path(), &["list"]));

    assert_eq!(list, Value::Array(vec![]), "別の置き場のページが見えた");
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[test]
fn キーチェーンを使えないosでキーチェーンを指定すると失敗する() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");

    let output = Command::new(env!("CARGO_BIN_EXE_nonoto"))
        .args(["--key-store", "keychain", "list"])
        .arg("--data-dir")
        .arg(dir.path())
        .output()
        .expect("コマンド nonoto を実行できなかった");

    assert_eq!(output.status.code(), Some(1), "終了コードが1ではない");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("鍵の保管場所にOSのキーチェーンを使えない"),
        "標準エラーにキーチェーンを使えない旨が無い: {stderr}"
    );
}
