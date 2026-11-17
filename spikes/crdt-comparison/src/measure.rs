//! 時間と大きさの計測と、必須の条件との照合。
//!
//! 条件の上限は、開発コンテナ (CPU 2コア) で5000行の文書を計った値で判定するために、計測の前に決めた見積もりである。

use std::time::{Duration, Instant};

use crate::data::{Dataset, Edit};
use crate::{Replica, ReplicaError};

/// 手元の編集1件・届いた更新1件の適用の、99パーセンタイルの上限。
pub const EDIT_LIMIT: Duration = Duration::from_millis(5);
/// スナップショットからの読み込みの、中央値の上限。
pub const LOAD_LIMIT: Duration = Duration::from_millis(100);

/// 編集1件の適用を計る回数。
pub const EDIT_RUNS: usize = 1000;
/// 読み込みを計る回数。中央値を取る。
pub const LOAD_RUNS: usize = 5;

/// 1文字の入力と、IMEの確定 (5文字) に当たる挿入。
const ONE_CHAR: &str = "あ";
const IME_COMMIT: &str = "日本語入力";

/// 時間の分布。
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub median: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl Timing {
    fn from_samples(mut samples: Vec<Duration>) -> Self {
        samples.sort_unstable();
        let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
        Self {
            median: at(0.5),
            p99: at(0.99),
            max: samples[samples.len() - 1],
        }
    }
}

/// 1つの候補の計測の結果。
pub struct Report {
    pub name: &'static str,
    /// 入力の列を当てて文書を作るまでの時間。
    pub build: Duration,
    pub local: Timing,
    pub remote: Timing,
    pub load: Duration,
    pub update_one_char: usize,
    pub update_ime: usize,
    pub snapshot: usize,
    /// 履歴をまとめたスナップショット。まとめる入口が無い候補は `None`。
    pub compacted_snapshot: Option<usize>,
    pub text_bytes: usize,
    pub version_2: usize,
    pub version_20: usize,
}

impl Report {
    /// 必須の条件ごとの判定。
    pub fn verdicts(&self) -> [(&'static str, bool); 3] {
        [
            ("手元の編集1件 (p99)", self.local.p99 <= EDIT_LIMIT),
            ("届いた更新1件 (p99)", self.remote.p99 <= EDIT_LIMIT),
            ("読み込み (中央値)", self.load <= LOAD_LIMIT),
        ]
    }

    pub fn passed(&self) -> bool {
        self.verdicts().iter().all(|(_, ok)| *ok)
    }
}

/// 空の文書に入力の列を当てて、計測の対象の文書を作る。
pub fn build<R: Replica>(dataset: &Dataset) -> Result<R, ReplicaError> {
    let mut doc = R::new();
    for edit in &dataset.edits {
        match edit {
            Edit::Insert { pos, text } => doc.insert(*pos, text)?,
            Edit::Delete { pos, len } => doc.delete(*pos, *len)?,
        };
    }
    Ok(doc)
}

/// `byte` 以降で最初の、文字の境界のバイト位置。
fn boundary_at(text: &str, byte: usize) -> usize {
    let mut pos = byte.min(text.len());
    while !text.is_char_boundary(pos) {
        pos += 1;
    }
    pos
}

/// 本文の中ほどにある、文字の境界のバイト位置。
fn middle(text: &str) -> usize {
    boundary_at(text, text.len() / 2)
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let value = f();
    (value, start.elapsed())
}

/// 読み込んだ文書を返し、計測が終わるまで破棄させない。
fn timed_load<T>(
    load: impl FnOnce() -> Result<T, ReplicaError>,
    read: impl FnOnce(&T) -> String,
) -> Result<(T, String, Duration), ReplicaError> {
    let (loaded, elapsed) = timed(|| {
        load().map(|loaded| {
            let text = read(&loaded);
            (loaded, text)
        })
    });
    let (loaded, text) = loaded?;
    Ok((loaded, text, elapsed))
}

/// `writers` 個の分岐でそれぞれ1文字を入れてマージしたあとの、版を指す値のバイト数。
fn version_size<R: Replica>(snapshot: &[u8], writers: usize) -> Result<usize, ReplicaError> {
    let mut base = R::from_snapshot(snapshot)?;
    let mut branches = (0..writers)
        .map(|_| base.fork())
        .collect::<Result<Vec<_>, _>>()?;
    for (i, branch) in branches.iter_mut().enumerate() {
        let text = branch.text();
        let pos = boundary_at(&text, text.len() * (i + 1) / (writers + 1));
        branch.insert(pos, ONE_CHAR)?;
    }
    for branch in &mut branches {
        base.merge(branch)?;
    }
    Ok(base.version().len())
}

/// 1つの候補を計る。計測の結果と、作った文書のスナップショットを返す。
pub fn run<R: Replica>(dataset: &Dataset) -> Result<(Report, Vec<u8>), ReplicaError> {
    let (doc, build) = timed(|| build::<R>(dataset));
    let mut doc = doc?;
    assert_eq!(
        doc.text(),
        dataset.text,
        "{}: 作った文書の本文が入力の列と違う",
        R::NAME
    );
    let snapshot = doc.snapshot();
    let text_bytes = dataset.text.len();

    let mut loads = Vec::with_capacity(LOAD_RUNS);
    for _ in 0..LOAD_RUNS {
        let (loaded, text, elapsed) = timed_load(|| R::from_snapshot(&snapshot), R::text)?;
        assert_eq!(
            text.len(),
            text_bytes,
            "{}: 読み込んだ本文の長さが違う",
            R::NAME
        );
        loads.push(elapsed);
        drop(loaded);
    }
    let load = Timing::from_samples(loads).median;

    let compacted_snapshot = doc.compacted_snapshot().map(|bytes| bytes.len());

    let pos = middle(&dataset.text);
    let update_one_char = R::from_snapshot(&snapshot)?.insert(pos, ONE_CHAR)?.len();
    let update_ime = R::from_snapshot(&snapshot)?.insert(pos, IME_COMMIT)?.len();

    let mut local = Vec::with_capacity(EDIT_RUNS);
    for _ in 0..EDIT_RUNS {
        let (update, elapsed) = timed(|| doc.insert(pos, ONE_CHAR));
        assert!(!update?.is_empty(), "{}: 更新が空", R::NAME);
        local.push(elapsed);
    }

    let mut remote_doc = doc.fork()?;
    let mut remote = Vec::with_capacity(EDIT_RUNS);
    for _ in 0..EDIT_RUNS {
        let update = remote_doc.insert(pos, ONE_CHAR)?;
        let (changes, elapsed) = timed(|| doc.apply_update_with_changes(&update));
        assert_eq!(changes?.len(), 1, "{}: 本文の変更が1件にならない", R::NAME);
        remote.push(elapsed);
    }
    assert_eq!(
        doc.text(),
        remote_doc.text(),
        "{}: 計測のあとに本文が一致しない",
        R::NAME
    );

    let report = Report {
        name: R::NAME,
        build,
        local: Timing::from_samples(local),
        remote: Timing::from_samples(remote),
        load,
        update_one_char,
        update_ime,
        snapshot: snapshot.len(),
        compacted_snapshot,
        text_bytes,
        version_2: version_size::<R>(&snapshot, 2)?,
        version_20: version_size::<R>(&snapshot, 20)?,
    };
    Ok((report, snapshot))
}

/// スナップショットからの読み込みで増えたメモリ (KiB)。測れないOSでは `None` を返す。
///
/// 計測の対象の文書を作るときのヒープを引き継がないよう、別のプロセスで呼ぶ。
pub fn load_memory_kib<R: Replica>(snapshot: &[u8]) -> Result<Option<u64>, ReplicaError> {
    let before = resident_kib();
    let loaded = R::from_snapshot(snapshot)?;
    let text = loaded.text();
    let after = resident_kib();
    std::hint::black_box((&loaded, &text));
    Ok(before
        .zip(after)
        .map(|(before, after)| after.saturating_sub(before)))
}

fn resident_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn 読み込んだ文書は計測終了後まで破棄されない() {
        struct Loaded<'a>(&'a Cell<bool>);

        impl Drop for Loaded<'_> {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }

        let dropped = Cell::new(false);
        let (loaded, text, _) = timed_load(|| Ok(Loaded(&dropped)), |_| "本文".to_owned()).unwrap();
        assert_eq!(text, "本文");
        assert!(!dropped.get(), "計測を終えて返す前に文書を破棄した");
        drop(loaded);
        assert!(dropped.get(), "文書が破棄されていない");
    }
}
