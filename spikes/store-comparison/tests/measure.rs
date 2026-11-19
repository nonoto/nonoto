use std::process::Command;

#[test]
fn データベースを消せないときは対象を伝えて失敗終了する() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let path = dir.path().join("a-base.db");
    std::fs::create_dir(&path).expect("データベースと同名のディレクトリを作れなかった");

    let output = Command::new(env!("CARGO_BIN_EXE_store-comparison"))
        .args(["--pages", "1", "--dir"])
        .arg(dir.path())
        .output()
        .expect("計測を起動できなかった");

    assert_eq!(
        output.status.code(),
        Some(1),
        "計測が失敗の終了コードを返さなかった"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("エラー: 前の計測のデータベースを消せなかった")
            && stderr.contains(path.to_string_lossy().as_ref()),
        "削除できなかったデータベースを伝えていない: {stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "エラーを1行で表示していない");
}
