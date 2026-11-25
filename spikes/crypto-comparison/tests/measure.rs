//! 計測のコマンドの引数と、最後まで結果を出すことを確かめる。

use std::process::Command;

#[test]
fn ゼロ回の計測は計測せずに拒否する() {
    let output = Command::new(env!("CARGO_BIN_EXE_crypto-comparison"))
        .args(["--runs", "0"])
        .output()
        .expect("計測を起動できなかった");

    assert!(!output.status.success(), "0回の計測で計測した");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--runs は1以上の整数で指定する"),
        "回数の制約を日本語で伝えていない"
    );
    assert!(output.stdout.is_empty(), "未計測の結果を出力した");
}

#[test]
fn 標本を確保できない回数は通常のエラーとして拒否する() {
    let output = Command::new(env!("CARGO_BIN_EXE_crypto-comparison"))
        .args(["--runs", &usize::MAX.to_string()])
        .output()
        .expect("計測を起動できなかった");

    assert_eq!(
        output.status.code(),
        Some(1),
        "計測が通常のエラー終了をしなかった"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("指定された計測回数の標本を確保できなかった"),
        "標本を確保できない理由を表示していない: {stderr}"
    );
    assert!(output.stdout.is_empty(), "未計測の結果を出力した");
}

#[test]
fn 少ない回数で全系統の結果を最後まで返す() {
    let output = Command::new(env!("CARGO_BIN_EXE_crypto-comparison"))
        .args(["--runs", "2"])
        .output()
        .expect("計測を起動できなかった");

    assert!(
        output.status.success(),
        "計測に失敗した: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("出力がUTF-8ではない");
    assert!(
        stdout.contains("操作ごとに2回計り"),
        "指定した回数で計測していない"
    );
    for section in [
        "## 必須の条件",
        "## 時間",
        "## 大きさ",
        "## 系統の間の受け渡し",
        "## AEADのnonce",
    ] {
        assert!(stdout.contains(section), "「{section}」の表が無い");
    }
    assert!(
        stdout.contains("| 判定 | 満たした | 満たした |"),
        "必須の条件を満たさない系統がある"
    );
}
