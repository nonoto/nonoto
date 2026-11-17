//! CRDTのライブラリの候補を、同じ入力の列で作った文書で計測する。
//!
//! 使い方: `cargo run --release -p crdt-comparison -- [--lines 行数]`
//! 結果はMarkdownで標準出力に書き、最後に候補ごとに必須の条件を満たしたかを表にする。

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use crdt_comparison::data::{self, Dataset};
use crdt_comparison::measure::{self, EDIT_LIMIT, EDIT_RUNS, LOAD_LIMIT, LOAD_RUNS, Report};
use crdt_comparison::{AutomergeReplica, LoroReplica, Replica, YrsReplica};

/// 入力の列を作る乱数の種。
const SEED: u64 = 2026;

struct Args {
    lines: usize,
    /// 別のプロセスで、読み込みで増えるメモリを計るときの候補の名前。
    measure_load: Option<String>,
}

impl Args {
    fn parse() -> Self {
        let mut args = Self {
            lines: 5000,
            measure_load: None,
        };
        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            let value = iter.next().unwrap_or_else(|| panic!("{arg} に値が無い"));
            match arg.as_str() {
                "--lines" => args.lines = value.parse().expect("--lines は整数で指定する"),
                "--measure-load" => args.measure_load = Some(value),
                _ => panic!("知らない引数: {arg}"),
            }
        }
        assert!(args.lines > 0, "--lines は1以上の整数で指定する");
        args
    }
}

/// 標準入力でスナップショットを受け取り、読み込みで増えたメモリ (KiB) を出力する。
fn run_load_worker(name: &str) {
    let mut snapshot = Vec::new();
    std::io::stdin()
        .read_to_end(&mut snapshot)
        .expect("スナップショットを受け取れなかった");
    let kib = match name {
        YrsReplica::NAME => measure::load_memory_kib::<YrsReplica>(&snapshot),
        AutomergeReplica::NAME => measure::load_memory_kib::<AutomergeReplica>(&snapshot),
        LoroReplica::NAME => measure::load_memory_kib::<LoroReplica>(&snapshot),
        _ => panic!("知らない候補: {name}"),
    }
    .expect("スナップショットを読み込めなかった");
    println!(
        "{}",
        kib.map_or_else(|| "-".to_owned(), |kib| kib.to_string())
    );
}

/// 文書を作るときのヒープを引き継がないよう、別のプロセスで読み込みのメモリを計る。
fn load_memory_in_new_process<R: Replica>(snapshot: &[u8]) -> Option<u64> {
    let mut child =
        Command::new(std::env::current_exe().expect("実行ファイルの場所を取得できなかった"))
            .args(["--measure-load", R::NAME])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("計測用のプロセスを起動できなかった");
    child
        .stdin
        .take()
        .expect("標準入力を開けなかった")
        .write_all(snapshot)
        .expect("スナップショットを渡せなかった");
    let output = child
        .wait_with_output()
        .expect("計測用のプロセスを待てなかった");
    assert!(output.status.success(), "計測用のプロセスが失敗した");
    String::from_utf8(output.stdout)
        .expect("計測用のプロセスの出力がUTF-8でない")
        .trim()
        .parse()
        .ok()
}

fn ms(duration: Duration) -> String {
    format!("{:.3}ms", duration.as_secs_f64() * 1000.0)
}

fn bytes(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.2}MiB", n as f64 / 1024.0 / 1024.0)
    } else if n >= 1024 {
        format!("{:.1}KiB", n as f64 / 1024.0)
    } else {
        format!("{n}B")
    }
}

fn ratio(n: usize, text: usize) -> String {
    format!("{:.2}倍", n as f64 / text as f64)
}

fn mark(ok: bool) -> &'static str {
    if ok {
        "満たした"
    } else {
        "満たさない"
    }
}

fn print_reports(dataset: &Dataset, reports: &[(Report, Option<u64>)]) {
    println!("# CRDTのライブラリの計測");
    println!();
    println!(
        "- 本文: {}行・{} (入力の列 {}件・種 {SEED})",
        dataset.lines(),
        bytes(dataset.text.len()),
        dataset.edits.len()
    );
    println!(
        "- 編集1件の適用: {EDIT_RUNS}回計り、99パーセンタイルで判定する (上限 {})",
        ms(EDIT_LIMIT)
    );
    println!(
        "- 読み込み: {LOAD_RUNS}回の中央値で判定する (上限 {})",
        ms(LOAD_LIMIT)
    );
    println!();

    let header: Vec<&str> = reports.iter().map(|(r, _)| r.name).collect();
    println!("| 計るもの | {} |", header.join(" | "));
    println!("| --- |{}", " --- |".repeat(header.len()));
    let row = |label: &str, f: &dyn Fn(&Report, Option<u64>) -> String| {
        let cells: Vec<String> = reports.iter().map(|(r, kib)| f(r, *kib)).collect();
        println!("| {label} | {} |", cells.join(" | "));
    };
    row("入力の列から文書を作る", &|r, _| ms(r.build));
    row("手元の編集1件 (中央値)", &|r, _| {
        ms(r.local.median)
    });
    row("手元の編集1件 (p99)", &|r, _| ms(r.local.p99));
    row("手元の編集1件 (最大)", &|r, _| ms(r.local.max));
    row("届いた更新1件 (中央値)", &|r, _| {
        ms(r.remote.median)
    });
    row("届いた更新1件 (p99)", &|r, _| ms(r.remote.p99));
    row("届いた更新1件 (最大)", &|r, _| ms(r.remote.max));
    row("読み込み (中央値)", &|r, _| ms(r.load));
    row("更新の大きさ (1文字)", &|r, _| {
        bytes(r.update_one_char)
    });
    row("更新の大きさ (IMEの確定5文字)", &|r, _| {
        bytes(r.update_ime)
    });
    row(
        "スナップショット (履歴をすべて持つ)",
        &|r, _| {
            format!(
                "{} ({})",
                bytes(r.snapshot),
                ratio(r.snapshot, r.text_bytes)
            )
        },
    );
    row(
        "スナップショット (履歴をまとめる)",
        &|r, _| {
            r.compacted_snapshot.map_or_else(
                || "入口が無い".to_owned(),
                |n| format!("{} ({})", bytes(n), ratio(n, r.text_bytes)),
            )
        },
    );
    row("読み込みで増えたメモリ", &|_, kib| {
        kib.map_or_else(|| "-".to_owned(), |kib| bytes(kib as usize * 1024))
    });
    row("版を指す値 (書き手2)", &|r, _| bytes(r.version_2));
    row("版を指す値 (書き手20)", &|r, _| bytes(r.version_20));
    println!();

    println!("## 必須の条件");
    println!();
    println!("| 候補 | 手元の編集1件 (p99) | 届いた更新1件 (p99) | 読み込み (中央値) | 判定 |");
    println!("| --- | --- | --- | --- | --- |");
    for (report, _) in reports {
        let verdicts = report.verdicts();
        println!(
            "| {} | {} | {} | {} | {} |",
            report.name,
            mark(verdicts[0].1),
            mark(verdicts[1].1),
            mark(verdicts[2].1),
            mark(report.passed())
        );
    }
}

fn measure_candidate<R: Replica>(dataset: &Dataset) -> (Report, Option<u64>) {
    eprintln!("{}を計測している", R::NAME);
    let (report, snapshot) = measure::run::<R>(dataset).expect("計測できなかった");
    let kib = load_memory_in_new_process::<R>(&snapshot);
    (report, kib)
}

fn main() {
    let args = Args::parse();
    if let Some(name) = args.measure_load {
        run_load_worker(&name);
        return;
    }
    let dataset = data::japanese_markdown(args.lines, SEED);
    let reports = vec![
        measure_candidate::<YrsReplica>(&dataset),
        measure_candidate::<AutomergeReplica>(&dataset),
        measure_candidate::<LoroReplica>(&dataset),
    ];
    print_reports(&dataset, &reports);
}
