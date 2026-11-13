//! 2つの暗号化ストアの方式を、同じページで計測する。
//!
//! 使い方: `cargo run --release -p store-comparison -- [--pages 件数] [--dir ディレクトリ]`
//! ディレクトリを省略すると一時ディレクトリを使う。

mod search;
mod startup;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use nonoto_core::{Key, Page, PageId, PageStore, PageUpdate, SqlCipherStore};
use rusqlite::Connection;
use store_comparison::record::RecordStore;
use zeroize::Zeroizing;

use startup::Scheme;

/// 検索の計測に使うクエリ。2文字・3文字以上・少数のページだけにある語を混ぜる。
const QUERIES: [&str; 4] = ["会議", "東京都", "議事録の確認", "見つからない語句"];

/// 平文が残っていないかを調べる目印。最初のページのタイトルに入れる。
const MARKER: &str = "比較用の目印のタイトル";

/// 計測を繰り返す回数。中央値を報告する。
const REPEAT: usize = 5;

/// 1件の保存・更新の計測に使う件数。
const SINGLE_OPS: usize = 100;

fn main() {
    let args = Args::parse();
    if let Some(scheme) = args.measure_open {
        startup::run_worker(scheme, &args.dir.expect("--measure-open には --dir が必要"));
        return;
    }
    let temp = tempfile_dir();
    let dir = args.dir.unwrap_or_else(|| temp.clone());
    std::fs::create_dir_all(&dir).expect("計測のディレクトリを作れなかった");
    let pages = generate_pages(args.pages);
    let total_chars: usize = pages.iter().map(|page| page.body.chars().count()).sum();
    println!(
        "ページ: {}件 (本文の平均 {}文字)",
        pages.len(),
        total_chars / pages.len().max(1)
    );

    let key = Key::generate().expect("鍵を作れなかった");
    measure_a(&dir.join("a.db"), &key, &pages);
    measure_b(&dir.join("b.db"), &key, &pages);

    if dir == temp {
        let _ = std::fs::remove_dir_all(&temp);
    }
}

struct Args {
    pages: usize,
    dir: Option<PathBuf>,
    measure_open: Option<Scheme>,
}

impl Args {
    fn parse() -> Self {
        let mut args = Self {
            pages: 10_000,
            dir: None,
            measure_open: None,
        };
        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            let value = iter.next().unwrap_or_else(|| panic!("{arg} に値が無い"));
            match arg.as_str() {
                "--pages" => args.pages = value.parse().expect("--pages は整数で指定する"),
                "--dir" => args.dir = Some(PathBuf::from(value)),
                "--measure-open" => args.measure_open = Some(Scheme::parse(&value)),
                _ => panic!("知らない引数: {arg}"),
            }
        }
        args
    }
}

fn tempfile_dir() -> PathBuf {
    std::env::temp_dir().join(format!("store-comparison-{}", std::process::id()))
}

fn measure_a(path: &Path, key: &Key, pages: &[Page]) {
    println!("\n## 方式A: データベース全体を暗号化する (SQLCipher)");
    remove_database(path);

    let mut store = SqlCipherStore::open(path, key).expect("方式Aのストアを開けなかった");
    let elapsed = time(|| insert_all(&mut store, pages));
    report("全件の保存 (1件ずつ)", elapsed);
    drop(store);

    report_startup(Scheme::A, path, key, "起動 (開く・鍵の確認)");

    let mut store = SqlCipherStore::open(path, key).expect("方式Aのストアを開けなかった");
    measure_single_ops(&mut store, pages, None);
    drop(store);

    let conn = open_sqlcipher(path, key);
    for query in QUERIES {
        let (found, elapsed) = median_with(|| {
            timed(|| search::scan(&conn, query).expect("方式Aを部分一致で検索できなかった"))
        });
        report(
            &format!("検索 部分一致の走査「{query}」 ({}件)", found.len()),
            elapsed,
        );
    }

    let size_before = database_size(path);
    let build =
        time(|| search::build_fts(&conn).expect("方式Aに全文検索のインデックスを作れなかった"));
    report("全文検索のインデックスの作成 (FTS5 trigram)", build);
    println!(
        "- 全文検索のインデックスで増えたファイルの大きさ: {} KiB",
        (database_size(path) - size_before) / 1024
    );
    for query in QUERIES.iter().filter(|query| query.chars().count() >= 3) {
        let (found, elapsed) = median_with(|| {
            timed(|| search::fts(&conn, query).expect("方式Aを全文検索できなかった"))
        });
        report(
            &format!("検索 FTS5「{query}」 ({}件)", found.len()),
            elapsed,
        );
    }
    drop(conn);

    report_files(path);

    // 全文検索で探す構成の保存と更新には、トリガーでインデックスを更新する時間が加わる。
    // ページの件数が変わるため、検索とファイルの大きさを計ったあとに計る。
    let mut store = SqlCipherStore::open(path, key).expect("方式Aのストアを開けなかった");
    measure_single_ops(&mut store, pages, Some("FTS5のインデックスを保つ"));
}

fn measure_b(path: &Path, key: &Key, pages: &[Page]) {
    println!("\n## 方式B: レコード単位で暗号化する (SQLite + XChaCha20-Poly1305)");
    remove_database(path);

    let mut store = RecordStore::open(path, key).expect("方式Bのストアを開けなかった");
    let elapsed = time(|| insert_all(&mut store, pages));
    report("全件の保存 (1件ずつ)", elapsed);
    drop(store);

    report_startup(
        Scheme::B,
        path,
        key,
        "起動 (開く・全件の復号・インデックスの作成)",
    );

    let mut store = RecordStore::open(path, key).expect("方式Bのストアを開けなかった");
    measure_single_ops(&mut store, pages, None);

    for query in QUERIES {
        let (found, elapsed) = median_with(|| timed(|| store.search(query)));
        report(
            &format!("検索 メモリ上のインデックス「{query}」 ({}件)", found.len()),
            elapsed,
        );
    }
    drop(store);

    report_files(path);
}

fn insert_all(store: &mut impl PageStore, pages: &[Page]) {
    for page in pages {
        store.insert(page).expect("ページを保存できなかった");
    }
}

/// 1件の保存と更新にかかる時間の平均を計る。`condition` は、計測の条件として見出しの括弧の中に添える。
fn measure_single_ops(store: &mut impl PageStore, pages: &[Page], condition: Option<&str>) {
    let note = |detail: String| match condition {
        Some(condition) => format!("{detail}、{condition}"),
        None => detail,
    };
    let extra = generate_pages(SINGLE_OPS);
    let elapsed = time(|| insert_all(store, &extra));
    report(
        &format!("1件の保存 ({})", note("平均".to_owned())),
        elapsed / SINGLE_OPS as u32,
    );

    let updates = measure_updates(store, pages);
    if let Some(average) = updates.average() {
        report(
            &format!(
                "1件の更新 ({})",
                note(format!("平均、{}件を計測", updates.count))
            ),
            average,
        );
    } else {
        println!("- 1件の更新: 計測対象無し ({})", note("0件".to_owned()));
    }
}

struct OperationMeasurement {
    elapsed: Duration,
    count: u32,
}

impl OperationMeasurement {
    fn average(&self) -> Option<Duration> {
        self.elapsed.checked_div(self.count)
    }
}

fn measure_updates(store: &mut impl PageStore, pages: &[Page]) -> OperationMeasurement {
    let now = Timestamp::now();
    let mut count = 0;
    let elapsed = time(|| {
        for page in pages.iter().take(SINGLE_OPS) {
            store
                .update(
                    page.id,
                    PageUpdate {
                        title: None,
                        body: Some(format!("{}\n追記した行", page.body)),
                    },
                    now,
                )
                .expect("ページを更新できなかった");
            count += 1;
        }
    });
    OperationMeasurement { elapsed, count }
}

fn open_sqlcipher(path: &Path, key: &Key) -> Connection {
    let conn = Connection::open(path).expect("方式Aのデータベースを開けなかった");
    let hex = Zeroizing::new(
        key.as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    );
    let statement = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", hex.as_str()));
    conn.execute_batch(&statement)
        .expect("方式Aのデータベースに鍵を渡せなかった");
    conn
}

/// データベースとジャーナルのファイルに目印の平文が無いかを調べ、大きさを報告する。
fn report_files(path: &Path) {
    let mut found = false;
    for file in database_files(path) {
        if let Ok(bytes) = std::fs::read(&file) {
            found |= bytes
                .windows(MARKER.len())
                .any(|window| window == MARKER.as_bytes());
        }
    }
    println!(
        "- ファイルの大きさ: {} KiB / 平文の目印: {}",
        database_size(path) / 1024,
        if found {
            "見つかった"
        } else {
            "見つからない"
        }
    );
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

fn remove_database(path: &Path) {
    for file in database_files(path) {
        let _ = std::fs::remove_file(file);
    }
}

fn report_startup(scheme: Scheme, path: &Path, key: &Key, label: &str) {
    let dir = path.parent().expect("データの置き場が無い");
    let (rss, elapsed) = median_with(|| {
        let measurement = startup::in_new_process(scheme, dir, key);
        (measurement.rss_increase_kib, measurement.elapsed)
    });
    report(label, elapsed);
    if let Some(kib) = rss {
        println!("- 初回オープンで増えたメモリ (RSS): {kib} KiB");
    }
}

fn report(label: &str, elapsed: Duration) {
    println!("- {label}: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
}

fn time(f: impl FnOnce()) -> Duration {
    let start = Instant::now();
    f();
    start.elapsed()
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let value = f();
    (value, start.elapsed())
}

fn median_with<T>(mut f: impl FnMut() -> (T, Duration)) -> (T, Duration) {
    let mut runs: Vec<(T, Duration)> = (0..REPEAT).map(|_| f()).collect();
    runs.sort_by_key(|(_, elapsed)| *elapsed);
    runs.swap_remove(REPEAT / 2)
}

/// 日本語のページを決まった順序で作る。
///
/// 「東京都」は100件に1件、「議事録の確認」は1000件に1件だけに入れ、少数のページだけにある語の検索を計る。
fn generate_pages(count: usize) -> Vec<Page> {
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
    let mut seed: u64 = 0x5eed;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as usize
    };
    let base = Timestamp::from_millisecond(1_700_000_000_000).expect("日時を作れなかった");
    (0..count)
        .map(|i| {
            let mut body: String = (0..400).map(|_| WORDS[next() % WORDS.len()]).collect();
            if i % 100 == 0 {
                body.push_str("東京都の拠点で");
            }
            if i % 1000 == 0 {
                body.push_str("議事録の確認をした");
            }
            let title = if i == 0 {
                MARKER.to_owned()
            } else {
                format!("ページ{i}")
            };
            let at = base
                .checked_add(jiff::SignedDuration::from_secs(i as i64))
                .expect("日時を作れなかった");
            Page {
                id: PageId::new(),
                title,
                body,
                created_at: at,
                updated_at: at,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 更新の平均の分母が実際に更新した件数になる() {
        for page_count in [0, 1, 120] {
            let key = Key::generate().expect("鍵を作れなかった");
            let mut store = SqlCipherStore::open(Path::new(":memory:"), &key)
                .expect("メモリ上のストアを開けなかった");
            let pages = generate_pages(page_count);
            insert_all(&mut store, &pages);

            let measured = measure_updates(&mut store, &pages);

            let mut changed = 0;
            for page in &pages {
                let saved = store
                    .get(page.id)
                    .expect("ページを取得できなかった")
                    .expect("保存したページが無い");
                if saved.body == format!("{}\n追記した行", page.body) {
                    changed += 1;
                } else {
                    assert_eq!(saved, *page, "対象外のページが変わった");
                }
            }
            assert_eq!(changed, page_count.min(SINGLE_OPS), "更新した件数が違う");
            assert_eq!(
                measured.count as usize, changed,
                "計測した件数と更新した件数が違う"
            );
            let expected = if changed == 0 {
                None
            } else {
                Some(measured.elapsed / changed as u32)
            };
            assert_eq!(
                measured.average(),
                expected,
                "更新の平均の分母が実際の件数ではない"
            );
        }
    }
}
