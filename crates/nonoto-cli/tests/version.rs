use std::process::Command;

#[test]
fn バージョンを表示する() {
    let output = Command::new(env!("CARGO_BIN_EXE_nonoto"))
        .arg("--version")
        .output()
        .expect("コマンド nonoto を実行できなかった");

    assert!(
        output.status.success(),
        "終了コードが成功ではない: {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("標準出力がUTF-8ではない");
    let want = format!("nonoto {}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(stdout, want, "標準出力が期待と異なる");
}
