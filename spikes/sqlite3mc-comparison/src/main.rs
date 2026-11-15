//! リンクしている暗号化の実装 (sqlite3mcかSQLCipher) で、ページの保存・起動・メモリ・1件の保存と更新を計る。
//!
//! 使い方: `cargo run --release -- [--pages 件数] [--dir ディレクトリ] [--baseline 実行ファイル]`
//! `--baseline` に、機能 `sqlcipher` でビルドしたこのプログラムを渡すと、同じ条件で実行させ、結果を同じ表に並べる。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sqlite3mc_comparison::{IMPLEMENTATION, Key, Page, Store};

/// 平文が残っていないかを調べる目印。最初のページのタイトルに入れる。
const MARKER: &str = "比較用の目印のタイトル";

/// 起動の計測を繰り返す回数。中央値を報告する。
const REPEAT: usize = 5;

/// 1件の保存・更新の計測に使う件数。
const SINGLE_OPS: usize = 100;

fn main() {
    let args = Args::parse();
    if let Some(path) = args.measure_open {
        run_worker(&path);
        return;
    }
    let temp = std::env::temp_dir().join(format!("sqlite3mc-comparison-{}", std::process::id()));
    let dir = args.dir.clone().unwrap_or_else(|| temp.clone());
    std::fs::create_dir_all(&dir).expect("計測のディレクトリを作れなかった");

    let row = measure(&dir.join(format!("{IMPLEMENTATION}.db")), args.pages);
    if args.row_only {
        println!("{}", row.to_line());
    } else {
        let mut rows = vec![row];
        if let Some(baseline) = &args.baseline {
            rows.push(run_baseline(baseline, &dir, args.pages));
        }
        print_table(args.pages, &rows);
    }

    if dir == temp {
        let _ = std::fs::remove_dir_all(&temp);
    }
}

struct Args {
    pages: usize,
    dir: Option<PathBuf>,
    baseline: Option<PathBuf>,
    measure_open: Option<PathBuf>,
    row_only: bool,
}

impl Args {
    fn parse() -> Self {
        let mut args = Self {
            pages: 10_000,
            dir: None,
            baseline: None,
            measure_open: None,
            row_only: false,
        };
        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            if arg == "--row" {
                args.row_only = true;
                continue;
            }
            let value = iter.next().unwrap_or_else(|| panic!("{arg} に値が無い"));
            match arg.as_str() {
                "--pages" => args.pages = value.parse().expect("--pages は整数で指定する"),
                "--dir" => args.dir = Some(PathBuf::from(value)),
                "--baseline" => args.baseline = Some(PathBuf::from(value)),
                "--measure-open" => args.measure_open = Some(PathBuf::from(value)),
                _ => panic!("知らない引数: {arg}"),
            }
        }
        args
    }
}

/// 1つの実装の計測の結果。
#[derive(Debug, PartialEq)]
struct Row {
    implementation: String,
    insert_all: Duration,
    startup: Duration,
    startup_rss_kib: Option<u64>,
    insert: Duration,
    update: Duration,
    file_kib: u64,
    plaintext_found: bool,
}

impl Row {
    /// 別のプロセスから受け取れるよう、タブ区切りの1行にする。
    fn to_line(&self) -> String {
        [
            self.implementation.clone(),
            self.insert_all.as_nanos().to_string(),
            self.startup.as_nanos().to_string(),
            self.startup_rss_kib
                .map(|kib| kib.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            self.insert.as_nanos().to_string(),
            self.update.as_nanos().to_string(),
            self.file_kib.to_string(),
            self.plaintext_found.to_string(),
        ]
        .join("\t")
    }

    fn from_line(line: &str) -> Self {
        let fields: Vec<&str> = line.trim().split('\t').collect();
        assert_eq!(fields.len(), 8, "計測の結果の形式が違う: {line}");
        let nanos =
            |field: &str| Duration::from_nanos(field.parse().expect("計測の時間が整数ではない"));
        Self {
            implementation: fields[0].to_owned(),
            insert_all: nanos(fields[1]),
            startup: nanos(fields[2]),
            startup_rss_kib: (fields[3] != "-")
                .then(|| fields[3].parse().expect("RSSが整数ではない")),
            insert: nanos(fields[4]),
            update: nanos(fields[5]),
            file_kib: fields[6].parse().expect("ファイルの大きさが整数ではない"),
            plaintext_found: fields[7].parse().expect("平文の有無が真偽値ではない"),
        }
    }
}

fn measure(path: &Path, count: usize) -> Row {
    remove_database(path);
    let key = Key::generate().expect("鍵を作れなかった");
    let pages = generate_pages(count, 0);

    let mut store = Store::open(path, &key).expect("ストアを開けなかった");
    let insert_all = time(|| {
        for page in &pages {
            store.insert(page).expect("ページを保存できなかった");
        }
    });
    drop(store);
    let file_kib = database_size(path) / 1024;
    let plaintext_found = contains_marker(path);

    let mut startups: Vec<(Duration, Option<u64>)> = (0..REPEAT)
        .map(|_| open_in_new_process(path, &key))
        .collect();
    startups.sort_by_key(|(elapsed, _)| *elapsed);
    let (startup, startup_rss_kib) = startups.swap_remove(REPEAT / 2);

    let mut store = Store::open(path, &key).expect("ストアを開けなかった");
    let extra = generate_pages(SINGLE_OPS, count);
    let insert = time(|| {
        for page in &extra {
            store.insert(page).expect("ページを保存できなかった");
        }
    }) / SINGLE_OPS as u32;
    let updated = pages.len().min(SINGLE_OPS);
    let update = time(|| {
        for page in pages.iter().take(SINGLE_OPS) {
            let body = format!("{}\n追記した行", page.body);
            let found = store
                .update_body(&page.id, &body, page.updated_at + 1)
                .expect("ページを更新できなかった");
            assert!(found, "更新するページが無い");
        }
    })
    .checked_div(updated as u32)
    .unwrap_or_default();
    drop(store);
    remove_database(path);

    Row {
        implementation: IMPLEMENTATION.to_owned(),
        insert_all,
        startup,
        startup_rss_kib,
        insert,
        update,
        file_kib,
        plaintext_found,
    }
}

/// 機能 `sqlcipher` でビルドしたこのプログラムを、同じ件数で実行させて結果を受け取る。
fn run_baseline(program: &Path, dir: &Path, pages: usize) -> Row {
    let output = Command::new(program)
        .args(["--row", "--pages", &pages.to_string(), "--dir"])
        .arg(dir)
        .stderr(Stdio::inherit())
        .output()
        .expect("比べる実装の計測を起動できなかった");
    assert!(output.status.success(), "比べる実装の計測が失敗した");
    Row::from_line(&String::from_utf8(output.stdout).expect("計測の結果がUTF-8ではない"))
}

fn print_table(pages: usize, rows: &[Row]) {
    println!("# 暗号化の実装の計測");
    println!("\n- ページ: {pages}件");
    println!(
        "\n| 実装 | 全件の保存 (1件ずつ) | 起動 (開く・鍵の確認) | 起動で増えたメモリ | 1件の保存 | 1件の更新 | ファイルの大きさ | 平文の目印 |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for row in rows {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} KiB | {} |",
            row.implementation,
            ms(row.insert_all),
            ms(row.startup),
            row.startup_rss_kib
                .map(|kib| format!("{kib} KiB"))
                .unwrap_or_else(|| "-".to_owned()),
            ms(row.insert),
            ms(row.update),
            row.file_kib,
            if row.plaintext_found {
                "見つかった"
            } else {
                "見つからない"
            }
        );
    }
}

/// 既存のデータベースを、データ投入で使ったヒープを引き継がない別のプロセスで初めて開く。
fn open_in_new_process(path: &Path, key: &Key) -> (Duration, Option<u64>) {
    let mut child =
        Command::new(std::env::current_exe().expect("実行ファイルの場所を取得できなかった"))
            .arg("--measure-open")
            .arg(path)
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
    (
        Duration::from_nanos(nanos.parse().expect("計測時間が整数ではない")),
        (rss != "-").then(|| rss.parse().expect("RSSが整数ではない")),
    )
}

/// 鍵を親から標準入力のパイプで受け取り、既存のデータベースを開く時間と、増えたメモリを出力する。
fn run_worker(path: &Path) {
    let mut bytes = [0u8; 32];
    std::io::stdin()
        .read_exact(&mut bytes)
        .expect("計測用の鍵を受け取れなかった");
    let key = Key::from_bytes(bytes);
    bytes.fill(0);
    assert!(path.is_file(), "計測するデータベースが無い");
    let before = resident_kib();
    let start = Instant::now();
    let store = Store::open(path, &key).expect("ストアを開けなかった");
    let elapsed = start.elapsed();
    let after = resident_kib();
    std::hint::black_box(&store);
    let rss = before
        .zip(after)
        .map(|(before, after)| after.saturating_sub(before).to_string())
        .unwrap_or_else(|| "-".to_owned());
    println!("{} {rss}", elapsed.as_nanos());
}

/// Linuxでは、このプロセスの常駐メモリ (KiB) を返す。
fn resident_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// 日本語のページを決まった順序で作る。`offset` は、ページの番号とIDの始まりである。
fn generate_pages(count: usize, offset: usize) -> Vec<Page> {
    const WORDS: [&str; 24] = [
        "会議",
        "設計",
        "予定",
        "資料",
        "共有",
        "実装",
        "テスト",
        "リリース",
        "今日は",
        "明日は",
        "について",
        "確認した",
        "進める",
        "決めた",
        "課題",
        "対応",
        "大阪",
        "名古屋",
        "の",
        "を",
        "が",
        "、",
        "。",
        "\n",
    ];
    let mut seed: u64 = 0x5eed + offset as u64;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as usize
    };
    (offset..offset + count)
        .map(|i| Page {
            id: format!("page-{i:08}"),
            title: if i == 0 {
                MARKER.to_owned()
            } else {
                format!("ページ{i}")
            },
            body: (0..400).map(|_| WORDS[next() % WORDS.len()]).collect(),
            updated_at: 1_700_000_000_000 + i as i64,
        })
        .collect()
}

fn database_files(path: &Path) -> Vec<PathBuf> {
    ["", "-wal", "-journal", "-shm"]
        .iter()
        .map(|suffix| PathBuf::from(format!("{}{suffix}", path.display())))
        .collect()
}

fn database_size(path: &Path) -> u64 {
    database_files(path)
        .iter()
        .filter_map(|file| std::fs::metadata(file).ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn contains_marker(path: &Path) -> bool {
    database_files(path).iter().any(|file| {
        std::fs::read(file).is_ok_and(|bytes| {
            bytes
                .windows(MARKER.len())
                .any(|window| window == MARKER.as_bytes())
        })
    })
}

fn remove_database(path: &Path) {
    for file in database_files(path) {
        let _ = std::fs::remove_file(file);
    }
}

fn time(f: impl FnOnce()) -> Duration {
    let start = Instant::now();
    f();
    start.elapsed()
}

fn ms(elapsed: Duration) -> String {
    format!("{:.3} ms", elapsed.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 計測の結果は1行にしても同じ値に戻る() {
        for rss in [Some(4_096), None] {
            let row = Row {
                implementation: "sqlite3mc".to_owned(),
                insert_all: Duration::from_millis(1_234),
                startup: Duration::from_micros(3_210),
                startup_rss_kib: rss,
                insert: Duration::from_nanos(12_345_678),
                update: Duration::from_nanos(9_876_543),
                file_kib: 34_700,
                plaintext_found: false,
            };

            assert_eq!(Row::from_line(&row.to_line()), row, "1行から戻した値が違う");
        }
    }
}
