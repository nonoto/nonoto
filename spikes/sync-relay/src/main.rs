//! 暗号化と署名を挟んだ同期のレコードの、時間と大きさを計る。
//!
//! 使い方: `cargo run --release -p sync-relay -- [--lines 行数] [--runs 回数]`
//! 結果はMarkdownで標準出力に書き、最後に必須の条件を満たしたかを書く。

use std::fmt::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use sync_relay::measure::{self, CompactionSize, LIMIT, Report, SEED, Size};

struct Args {
    lines: usize,
    runs: usize,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Self {
            lines: 5000,
            runs: 1000,
        };
        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            let value = iter.next().ok_or_else(|| format!("{arg} に値が無い"))?;
            let number = value
                .parse::<usize>()
                .ok()
                .filter(|number| *number > 0)
                .ok_or_else(|| format!("{arg} は1以上の整数で指定する"))?;
            match arg.as_str() {
                "--lines" => args.lines = number,
                "--runs" => args.runs = number,
                _ => return Err(format!("知らない引数: {arg}")),
            }
        }
        Ok(args)
    }
}

fn ms(duration: Duration) -> String {
    format!("{:.3}ms", duration.as_secs_f64() * 1000.0)
}

fn size_row(out: &mut String, name: &str, size: Size) {
    let _ = writeln!(
        out,
        "| {name} | {}バイト | {}バイト | {}バイト |",
        size.payload,
        size.record,
        size.overhead()
    );
}

fn compaction_row(out: &mut String, name: &str, size: CompactionSize) {
    let _ = writeln!(
        out,
        "| {name} | {}バイト | {}バイト | {}バイト |",
        size.document, size.contents, size.record
    );
}

fn render(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# 同期のレコードの計測\n");
    let _ = writeln!(
        out,
        "- 計測開始時の文書: {}行・{}バイト (入力の列 {}件・種 {SEED})",
        report.lines, report.text_bytes, report.edits
    );
    let _ = writeln!(
        out,
        "- 追加編集後の文書: {}バイト。スナップショットと版はこの時点で計測",
        report.final_text_bytes
    );
    let _ = writeln!(
        out,
        "- 回数: 手元の編集と届いた更新を、それぞれ{}回",
        report.runs
    );
    let _ = writeln!(out, "- 上限: p99で{}\n", ms(LIMIT));

    let _ = writeln!(out, "## 時間\n");
    let _ = writeln!(out, "| 操作 | 中央値 | p99 | 最大 |");
    let _ = writeln!(out, "| --- | --- | --- | --- |");
    for (name, timing) in [
        (
            "手元の編集1件 (更新の取り出し・著者の署名・暗号化・書き込みの署名)",
            report.local,
        ),
        (
            "届いた更新1件 (中身の検査の規則で確かめて当てる)",
            report.remote,
        ),
    ] {
        let _ = writeln!(
            out,
            "| {name} | {} | {} | {} |",
            ms(timing.median),
            ms(timing.p99),
            ms(timing.max)
        );
    }
    let _ = writeln!(out, "\n計測した最大値の変動要因は未特定。");

    let _ = writeln!(out, "\n## 大きさ\n");
    let _ = writeln!(
        out,
        "| レコード | 中身 | レコード | 暗号化と署名で増えた分 |"
    );
    let _ = writeln!(out, "| --- | --- | --- | --- |");
    size_row(&mut out, "更新1件 (1文字の入力)", report.update);
    size_row(&mut out, "スナップショット (文書の全体)", report.snapshot);
    size_row(&mut out, "版の記録 (中身は版の値)", report.version);
    size_row(
        &mut out,
        "世代の記録 (メンバー4人。中身は包んだ鍵の合計)",
        report.generation,
    );

    let _ = writeln!(out, "\n## まとめの大きさ\n");
    let _ = writeln!(
        out,
        "版を2つ (計測を始める前と、追加編集の後) 付けた本文のログを、版の中身の残し方ごとにまとめた大きさ。\n"
    );
    let _ = writeln!(
        out,
        "| 版の中身の残し方 | 文書のスナップショット | 別に持つ版の中身 | レコード |"
    );
    let _ = writeln!(out, "| --- | --- | --- | --- |");
    compaction_row(&mut out, "版より前の履歴を残す", report.compacted_history);
    compaction_row(
        &mut out,
        "版の時点の中身を別に持つ (履歴を省く)",
        report.compacted_contents,
    );
    let _ = writeln!(out, "\n## 必須の条件\n");
    let _ = writeln!(
        out,
        "手元の編集1件と届いた更新1件が、それぞれp99で{}以内: {}",
        ms(LIMIT),
        if report.passed() {
            "満たした"
        } else {
            "満たさなかった"
        }
    );
    out
}

fn main() -> ExitCode {
    let args = match Args::parse() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("エラー: {message}");
            return ExitCode::FAILURE;
        }
    };
    match measure::run(args.lines, args.runs) {
        Ok(report) => {
            print!("{}", render(&report));
            ExitCode::SUCCESS
        }
        Err(error) => {
            let mut message = error.to_string();
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                let _ = write!(message, ": {cause}");
                source = cause.source();
            }
            eprintln!("エラー: {message}");
            ExitCode::FAILURE
        }
    }
}
