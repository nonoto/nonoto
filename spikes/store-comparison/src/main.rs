//! 暗号化ストアの方式と、方式Aの全文検索のインデックスの変種を、同じページで計測する。
//!
//! 使い方: `cargo run --release -p store-comparison -- [--pages 件数] [--dir ディレクトリ]`
//! ディレクトリを省略すると一時ディレクトリを使う。
//! 結果はMarkdownで標準出力に書き、最後に変種ごとに必須の条件を満たしたかを表にする。

// `unsafe` はライブラリの `fts5` だけに置き、このクレートでは許さない。
#![deny(unsafe_code)]

mod data;
mod search;
mod startup;
mod verdict;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use nonoto_core::{Error, Key, Page, PageId, PageStore, PageSummary, PageUpdate, SqlCipherStore};
use rusqlite::Connection;
use store_comparison::record::RecordStore;
use store_comparison::tokenizer::Tokenizer;
use zeroize::Zeroizing;

use data::{MARKER, QUERIES};
use search::{ConnectionStore, FtsIndex};
use startup::Scheme;
use verdict::{Measured, judge, mark};

/// 計測を繰り返す回数。中央値を報告する。
const REPEAT: usize = 5;

/// 1件の保存・更新の計測に使う件数。
const SINGLE_OPS: usize = 100;

/// 方式Aで、ページをどう探すか。
#[derive(Clone, Copy)]
enum Lookup {
    /// インデックスを作らず、部分一致で全件を走査する。
    Scan,
    Fts(FtsIndex),
}

const LOOKUPS: [Lookup; 4] = [
    Lookup::Scan,
    Lookup::Fts(FtsIndex::Trigram),
    Lookup::Fts(FtsIndex::Custom(Tokenizer::Bigram)),
    Lookup::Fts(FtsIndex::Custom(Tokenizer::Unigram)),
];

impl Lookup {
    fn label(self) -> &'static str {
        match self {
            Self::Scan => "方式A・走査",
            Self::Fts(FtsIndex::Trigram) => "方式A・trigram",
            Self::Fts(FtsIndex::Custom(Tokenizer::Bigram)) => "方式A・バイグラム",
            Self::Fts(FtsIndex::Custom(Tokenizer::Unigram)) => "方式A・ユニグラム",
        }
    }

    fn file_name(self) -> String {
        match self {
            Self::Scan => "a-scan.db".to_owned(),
            Self::Fts(index) => format!("a-{}.db", index.tokenizer_name()),
        }
    }

    /// 2文字の語を全件の走査に落とさずに引くか。trigramは3文字未満を走査で探す (`search::fts`)。
    fn indexes_two_chars(self) -> bool {
        matches!(self, Self::Fts(FtsIndex::Custom(_)))
    }

    fn search(self, conn: &Connection, query: &str) -> Result<Vec<PageSummary>, Error> {
        match self {
            Self::Scan => search::scan(conn, query),
            Self::Fts(index) => search::fts(conn, index, query),
        }
    }
}

/// 1つの変種の計測の結果。最後の表に使う。
struct Summary {
    label: &'static str,
    measured: Measured,
    /// 1文字のクエリの検索の時間。
    single_char_search: Duration,
    /// 判定に含めるクエリのうち、最も遅かった検索の時間。
    slowest_search: Duration,
    index_build: Option<Duration>,
    index_kib: Option<u64>,
    file_kib: u64,
}

fn main() {
    let args = Args::parse();
    if let Some(scheme) = args.measure_open {
        startup::run_worker(
            scheme,
            &args.path.expect("--measure-open には --path が必要"),
        );
        return;
    }
    let temp = tempfile_dir();
    let dir = args.dir.unwrap_or_else(|| temp.clone());
    std::fs::create_dir_all(&dir).expect("計測のディレクトリを作れなかった");
    let pages = data::generate_pages(args.pages);
    let total_chars: usize = pages.iter().map(|page| page.body.chars().count()).sum();
    println!("# 暗号化ストアの計測");
    println!(
        "\n- ページ: {}件 (本文の平均 {}文字)",
        pages.len(),
        total_chars / pages.len().max(1)
    );
    println!(
        "- CPU: {}",
        std::thread::available_parallelism()
            .map(|count| format!("{count}コア"))
            .unwrap_or_else(|_| "不明".to_owned())
    );

    let key = Key::generate().expect("鍵を作れなかった");
    let base = dir.join("a-base.db");
    let expected = prepare_base(&base, &key, &pages);
    let mut summaries: Vec<Summary> = LOOKUPS
        .iter()
        .map(|lookup| measure_a(*lookup, &base, &dir, &key, &pages, &expected))
        .collect();
    remove_database(&base);
    summaries.push(measure_b(&dir.join("b.db"), &key, &pages, &expected));

    print_summary(&summaries);

    if dir == temp {
        let _ = std::fs::remove_dir_all(&temp);
    }
}

struct Args {
    pages: usize,
    dir: Option<PathBuf>,
    measure_open: Option<Scheme>,
    path: Option<PathBuf>,
}

impl Args {
    fn parse() -> Self {
        let mut args = Self {
            pages: 10_000,
            dir: None,
            measure_open: None,
            path: None,
        };
        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            let value = iter.next().unwrap_or_else(|| panic!("{arg} に値が無い"));
            match arg.as_str() {
                "--pages" => args.pages = value.parse().expect("--pages は整数で指定する"),
                "--dir" => args.dir = Some(PathBuf::from(value)),
                "--measure-open" => args.measure_open = Some(Scheme::parse(&value)),
                "--path" => args.path = Some(PathBuf::from(value)),
                _ => panic!("知らない引数: {arg}"),
            }
        }
        args
    }
}

fn tempfile_dir() -> PathBuf {
    std::env::temp_dir().join(format!("store-comparison-{}", std::process::id()))
}

/// 方式Aの変種で共有する、インデックスの無いデータベースを作る。
///
/// 全件の保存の時間はインデックスの有無で変わらないため、ここで1回だけ計る。
/// クエリごとに、部分一致の走査で見つかるページを返す。各変種の結果はこれと比べる。
fn prepare_base(path: &Path, key: &Key, pages: &[Page]) -> Vec<Vec<PageId>> {
    println!("\n## 方式A: データベース全体を暗号化する (SQLCipher)");
    remove_database(path);
    let mut store = SqlCipherStore::open(path, key).expect("方式Aのストアを開けなかった");
    let elapsed = time(|| insert_all(&mut store, pages));
    report("全件の保存 (1件ずつ、インデックス無し)", elapsed);
    drop(store);
    let conn = open_sqlcipher(path, key);
    QUERIES
        .iter()
        .map(|query| {
            ids(search::scan(&conn, query.text).expect("方式Aを部分一致で検索できなかった"))
        })
        .collect()
}

fn measure_a(
    lookup: Lookup,
    base: &Path,
    dir: &Path,
    key: &Key,
    pages: &[Page],
    expected: &[Vec<PageId>],
) -> Summary {
    println!("\n## {}", lookup.label());
    let path = dir.join(lookup.file_name());
    remove_database(&path);
    std::fs::copy(base, &path).expect("方式Aのデータベースを複製できなかった");

    let conn = open_sqlcipher(&path, key);
    let (index_build, index_kib) = match lookup {
        Lookup::Scan => (None, None),
        Lookup::Fts(index) => {
            let size_before = database_size(&path);
            let build = time(|| {
                search::build_fts(&conn, index).expect("全文検索のインデックスを作れなかった")
            });
            report("全文検索のインデックスの作成", build);
            let kib = (database_size(&path) - size_before) / 1024;
            println!("- 全文検索のインデックスで増えたファイルの大きさ: {kib} KiB");
            (Some(build), Some(kib))
        }
    };
    let file_kib = report_files(&path);
    let searches = measure_searches(expected, |query| {
        lookup
            .search(&conn, query)
            .expect("方式Aを検索できなかった")
    });
    drop(conn);

    let startup = report_startup(Scheme::A, &path, key, "起動 (開く・鍵の確認)");
    let mut store = ConnectionStore::new(open_sqlcipher(&path, key));
    let condition = match lookup {
        Lookup::Scan => "インデックス無し",
        Lookup::Fts(_) => "全文検索のインデックスを保つ",
    };
    let writes = measure_single_ops(&mut store, pages, condition);
    drop(store);

    summarize(
        lookup.label(),
        lookup.indexes_two_chars(),
        searches,
        startup,
        writes,
        (index_build, index_kib, file_kib),
    )
}

fn measure_b(path: &Path, key: &Key, pages: &[Page], expected: &[Vec<PageId>]) -> Summary {
    println!("\n## 方式B: レコード単位で暗号化する (SQLite + XChaCha20-Poly1305)");
    remove_database(path);

    let mut store = RecordStore::open(path, key).expect("方式Bのストアを開けなかった");
    let elapsed = time(|| insert_all(&mut store, pages));
    report("全件の保存 (1件ずつ)", elapsed);
    drop(store);
    let file_kib = report_files(path);

    let startup = report_startup(
        Scheme::B,
        path,
        key,
        "起動 (開く・全件の復号・インデックスの作成)",
    );

    let mut store = RecordStore::open(path, key).expect("方式Bのストアを開けなかった");
    let searches = measure_searches(expected, |query| store.search(query));
    let writes = measure_single_ops(&mut store, pages, "メモリ上のインデックスを保つ");
    drop(store);

    summarize(
        "方式B",
        true,
        searches,
        startup,
        writes,
        (None, None, file_kib),
    )
}

/// クエリごとの検索の時間 (中央値) を計る。結果が部分一致の走査と違えば止める。
fn measure_searches(
    expected: &[Vec<PageId>],
    mut search: impl FnMut(&str) -> Vec<PageSummary>,
) -> Vec<Duration> {
    QUERIES
        .iter()
        .zip(expected)
        .map(|(query, expected)| {
            let (found, elapsed) = median_with(|| timed(|| search(query.text)));
            assert_eq!(
                &ids(found),
                expected,
                "検索の結果が部分一致の走査と異なる: {}",
                query.text
            );
            report(
                &format!(
                    "検索 {}「{}」 ({}件)",
                    query.label,
                    query.text.escape_debug(),
                    expected.len()
                ),
                elapsed,
            );
            elapsed
        })
        .collect()
}

fn summarize(
    label: &'static str,
    indexes_two_chars: bool,
    searches: Vec<Duration>,
    startup: startup::Measurement,
    (insert, update): (Duration, Option<Duration>),
    (index_build, index_kib, file_kib): (Option<Duration>, Option<u64>, u64),
) -> Summary {
    let required_searches: Vec<Duration> = QUERIES
        .iter()
        .zip(&searches)
        .filter(|(query, _)| query.required)
        .map(|(_, elapsed)| *elapsed)
        .collect();
    let single_char_search = QUERIES
        .iter()
        .zip(&searches)
        .find(|(query, _)| query.text.chars().count() == 1)
        .map(|(_, elapsed)| *elapsed)
        .expect("1文字のクエリが無い");
    Summary {
        label,
        slowest_search: required_searches.iter().copied().max().unwrap_or_default(),
        single_char_search,
        measured: Measured {
            indexes_two_chars,
            required_searches,
            startup: startup.elapsed,
            startup_rss_kib: startup.rss_increase_kib,
            insert,
            update,
        },
        index_build,
        index_kib,
        file_kib,
    }
}

fn print_summary(summaries: &[Summary]) {
    println!("\n## 必須の条件");
    let verdicts: Vec<_> = summaries
        .iter()
        .map(|summary| judge(&summary.measured))
        .collect();
    let conditions: Vec<_> = verdicts[0]
        .conditions
        .iter()
        .map(|(name, _)| *name)
        .collect();
    println!("\n| 変種 | {} | すべて |", conditions.join(" | "));
    println!("|---{}|---|", "|---".repeat(conditions.len()));
    for (summary, verdict) in summaries.iter().zip(&verdicts) {
        let marks: Vec<_> = verdict
            .conditions
            .iter()
            .map(|(_, result)| mark(*result))
            .collect();
        println!(
            "| {} | {} | {} |",
            summary.label,
            marks.join(" | "),
            mark(verdict.passed())
        );
    }

    println!("\n## 計測の値");
    println!(
        "\n| 変種 | 検索 (必須のクエリの最大) | 検索 (1文字) | 起動 | 起動で増えたメモリ | 1件の保存 | 1件の更新 | インデックスの作成 | インデックスの大きさ | ファイルの大きさ |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    let kib = |value: u64| format!("{value} KiB");
    let or_dash = |value: Option<String>| value.unwrap_or_else(|| "-".to_owned());
    for summary in summaries {
        let measured = &summary.measured;
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            summary.label,
            ms(summary.slowest_search),
            ms(summary.single_char_search),
            ms(measured.startup),
            or_dash(measured.startup_rss_kib.map(kib)),
            ms(measured.insert),
            or_dash(measured.update.map(ms)),
            or_dash(summary.index_build.map(ms)),
            or_dash(summary.index_kib.map(kib)),
            kib(summary.file_kib),
        );
    }
}

fn ids(summaries: Vec<PageSummary>) -> Vec<PageId> {
    summaries.into_iter().map(|summary| summary.id).collect()
}

fn insert_all(store: &mut impl PageStore, pages: &[Page]) {
    for page in pages {
        store.insert(page).expect("ページを保存できなかった");
    }
}

/// 1件の保存・更新を計るストア。方式Aはトークナイザーを登録した接続から、方式Bはストアから保存する。
trait Writes {
    fn insert(&mut self, page: &Page) -> Result<(), Error>;
    fn update(&mut self, id: PageId, update: PageUpdate, now: Timestamp) -> Result<(), Error>;
}

impl Writes for ConnectionStore {
    fn insert(&mut self, page: &Page) -> Result<(), Error> {
        ConnectionStore::insert(self, page)
    }

    fn update(&mut self, id: PageId, update: PageUpdate, now: Timestamp) -> Result<(), Error> {
        ConnectionStore::update(self, id, update, now)
    }
}

impl Writes for RecordStore {
    fn insert(&mut self, page: &Page) -> Result<(), Error> {
        PageStore::insert(self, page)
    }

    fn update(&mut self, id: PageId, update: PageUpdate, now: Timestamp) -> Result<(), Error> {
        PageStore::update(self, id, update, now).map(|_| ())
    }
}

/// 1件の保存と更新にかかる時間の平均を計る。`condition` は、計測の条件として見出しの括弧の中に添える。
fn measure_single_ops(
    store: &mut impl Writes,
    pages: &[Page],
    condition: &str,
) -> (Duration, Option<Duration>) {
    let extra = data::generate_pages(SINGLE_OPS);
    let elapsed = time(|| {
        for page in &extra {
            store.insert(page).expect("ページを保存できなかった");
        }
    });
    let insert = elapsed / SINGLE_OPS as u32;
    report(&format!("1件の保存 (平均、{condition})"), insert);

    let updates = measure_updates(store, pages);
    let update = updates.average();
    if let Some(average) = update {
        report(
            &format!("1件の更新 (平均、{}件を計測、{condition})", updates.count),
            average,
        );
    } else {
        println!("- 1件の更新: 計測対象無し (0件、{condition})");
    }
    (insert, update)
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

fn measure_updates(store: &mut impl Writes, pages: &[Page]) -> OperationMeasurement {
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

/// 方式Aのデータベースを開いて鍵を渡し、全文検索のトークナイザーを登録する。
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
    store_comparison::fts5::register(&conn).expect("全文検索のトークナイザーを登録できなかった");
    conn
}

/// データベースとジャーナルのファイルに目印の平文が無いかを調べ、大きさ (KiB) を報告する。
fn report_files(path: &Path) -> u64 {
    let mut found = false;
    for file in database_files(path) {
        if let Ok(bytes) = std::fs::read(&file) {
            found |= bytes
                .windows(MARKER.len())
                .any(|window| window == MARKER.as_bytes());
        }
    }
    let kib = database_size(path) / 1024;
    println!(
        "- ファイルの大きさ: {kib} KiB / 平文の目印: {}",
        if found {
            "見つかった"
        } else {
            "見つからない"
        }
    );
    kib
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

fn report_startup(scheme: Scheme, path: &Path, key: &Key, label: &str) -> startup::Measurement {
    let (rss, elapsed) = median_with(|| {
        let measurement = startup::in_new_process(scheme, path, key);
        (measurement.rss_increase_kib, measurement.elapsed)
    });
    report(label, elapsed);
    if let Some(kib) = rss {
        println!("- 初回オープンで増えたメモリ (RSS): {kib} KiB");
    }
    startup::Measurement {
        elapsed,
        rss_increase_kib: rss,
    }
}

fn report(label: &str, elapsed: Duration) {
    println!("- {label}: {}", ms(elapsed));
}

fn ms(elapsed: Duration) -> String {
    format!("{:.3} ms", elapsed.as_secs_f64() * 1000.0)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 全文検索のインデックスを作った方式Aのデータベースと、それに保存・更新する接続を用意する。
    fn indexed_store(dir: &Path, key: &Key, pages: &[Page]) -> (ConnectionStore, Connection) {
        let path = dir.join("a.db");
        insert_all(
            &mut SqlCipherStore::open(&path, key).expect("方式Aを開けなかった"),
            pages,
        );
        let conn = open_sqlcipher(&path, key);
        search::build_fts(&conn, FtsIndex::Custom(Tokenizer::Bigram))
            .expect("全文検索のインデックスを作れなかった");
        (ConnectionStore::new(open_sqlcipher(&path, key)), conn)
    }

    #[test]
    fn 更新の平均の分母が実際に更新した件数になる() {
        for page_count in [0, 1, 120] {
            let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
            let key = Key::generate().expect("鍵を作れなかった");
            let pages = data::generate_pages(page_count);
            let (mut store, conn) = indexed_store(dir.path(), &key, &pages);

            let measured = measure_updates(&mut store, &pages);

            let changed: i64 = conn
                .query_row(
                    "SELECT count(*) FROM pages WHERE body LIKE '%' || char(10) || '追記した行'",
                    [],
                    |row| row.get(0),
                )
                .expect("更新したページを数えられなかった");
            assert_eq!(
                changed as usize,
                page_count.min(SINGLE_OPS),
                "更新した件数が違う"
            );
            assert_eq!(
                measured.count as usize, changed as usize,
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

    #[test]
    fn 一件の保存と更新の計測でもインデックスを保つ() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let key = Key::generate().expect("鍵を作れなかった");
        let pages = data::generate_pages(3);
        let (mut store, conn) = indexed_store(dir.path(), &key, &pages);

        measure_single_ops(&mut store, &pages, "テスト");

        let count: i64 = conn
            .query_row("SELECT count(*) FROM pages", [], |row| row.get(0))
            .expect("ページを数えられなかった");
        assert_eq!(
            count as usize,
            pages.len() + SINGLE_OPS,
            "保存したページの件数が違う"
        );
        conn.execute(
            "INSERT INTO pages_fts(pages_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .expect("全文検索のインデックスがページの内容と食い違う");
    }

    #[test]
    fn 二文字の語をインデックスで引くのは自作のトークナイザーだけ() {
        let indexed: Vec<_> = LOOKUPS
            .iter()
            .filter(|lookup| lookup.indexes_two_chars())
            .map(|lookup| lookup.label())
            .collect();

        assert_eq!(
            indexed,
            ["方式A・バイグラム", "方式A・ユニグラム"],
            "2文字をインデックスで引く変種が期待と異なる"
        );
    }
}
