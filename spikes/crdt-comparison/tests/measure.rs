//! 計測のコマンドの引数と、最後まで結果を出すことを確かめる。

use std::process::Command;

#[test]
fn ゼロ行の入力は計測せずに拒否する() {
    let output = Command::new(env!("CARGO_BIN_EXE_crdt-comparison"))
        .args(["--lines", "0"])
        .output()
        .expect("計測を起動できなかった");

    assert!(!output.status.success(), "0行の入力で計測した");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--lines は1以上の整数で指定する"),
        "行数の制約を日本語で伝えていない"
    );
    assert!(
        !stderr.contains("を計測している"),
        "入力を拒否する前に計測を始めた"
    );
    assert!(output.stdout.is_empty(), "未計測の結果を出力した");
}

#[test]
fn 少ない行数で全候補の計測結果を最後まで返す() {
    let output = Command::new(env!("CARGO_BIN_EXE_crdt-comparison"))
        .args(["--lines", "3"])
        .output()
        .expect("計測を起動できなかった");

    assert!(
        output.status.success(),
        "計測に失敗した: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("出力がUTF-8ではない");
    assert!(stdout.contains("本文: 3行"), "指定した行数で計測していない");
    assert!(stdout.contains("## 必須の条件"), "最後の判定の表が無い");
    for candidate in ["yrs", "automerge", "loro"] {
        assert!(
            stdout.contains(&format!("| {candidate} |")),
            "{candidate}の判定が無い"
        );
    }
    assert!(!stdout.contains("inf倍"), "倍率が無限大になった");
    assert!(!stdout.contains("NaN倍"), "倍率が非数になった");
}
