use std::process::Command;

#[test]
fn ゼロ件の入力は計測せずに拒否する() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");

    let output = Command::new(env!("CARGO_BIN_EXE_sqlite3mc-comparison"))
        .args(["--row", "--pages", "0", "--dir"])
        .arg(dir.path())
        .output()
        .expect("計測を起動できなかった");

    assert!(!output.status.success(), "0件の入力で計測した");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--pages は1以上の整数で指定する"),
        "件数の制約を日本語で伝えていない"
    );
    assert!(output.stdout.is_empty(), "未計測の結果を出力した");
    assert_eq!(
        std::fs::read_dir(dir.path())
            .expect("ディレクトリを読めなかった")
            .count(),
        0,
        "入力を拒否する前にデータベースを作った"
    );
}

#[test]
fn 少ない件数で計測を最後まで行い結果を1行で返す() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");

    let output = Command::new(env!("CARGO_BIN_EXE_sqlite3mc-comparison"))
        .args(["--row", "--pages", "3", "--dir"])
        .arg(dir.path())
        .output()
        .expect("計測を起動できなかった");

    assert!(
        output.status.success(),
        "計測に失敗した: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("出力がUTF-8ではない");
    let fields: Vec<_> = stdout.trim().split('\t').collect();
    assert_eq!(fields.len(), 8, "結果の形式が違う: {stdout}");
    assert_eq!(fields[7], "false", "ファイルに平文の目印が残った");
    assert_eq!(
        std::fs::read_dir(dir.path())
            .expect("ディレクトリを読めなかった")
            .count(),
        0,
        "計測のデータベースが残った"
    );
}
