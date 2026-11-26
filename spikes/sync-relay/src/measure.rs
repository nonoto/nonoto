//! 暗号化と署名を挟んだ同期のレコードの、時間と大きさの計測。
//!
//! 文書は、同時編集の技術検証の計測と同じく、固定した種の乱数の入力の列で、日本語のMarkdownを作る。
//! 上限 (p99で5ms) は、計測の前に決めた見積もりである。

use std::collections::TryReserveError;
use std::time::{Duration, Instant};

use crdt_comparison::data::{self, Dataset, Edit};
use crdt_comparison::rng::SeededRng;
use nonoto_core::{
    Branch, CheckedEntry, CheckedVersion, Document, GenerationKeys, GenerationRecord,
    GenerationView, Member, MemberId, Membership, RecordHeader, RecordKind, Role, SealedRecord,
    Suite, SyncPageId, TopicId, UserKeys, VersionRetention, Writer, check_log_entry,
    check_snapshot,
};

/// 入力の列を作る乱数の種。同時編集の技術検証の計測と同じ値にする。
pub const SEED: u64 = 2026;

/// 手元の編集1件と、届いた更新1件の、p99の上限。
pub const LIMIT: Duration = Duration::from_millis(5);

/// 手元の編集1件で入れる文字。
const ONE_CHAR: &str = "あ";

/// メンバー4人の世代の記録で包む鍵の合計。4人のトピックの鍵 (32バイト) と、
/// 編集2人の2つの書き込みの鍵と提案1人の1つの書き込みの鍵の種 (32バイト)。
const WRAPPED_KEYS: usize = 4 * 32 + (2 * 2 + 1) * 32;

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
        // 百分位の位置を、整数で四捨五入して求める。
        let at = |percent: usize| samples[((samples.len() - 1) * percent + 50) / 100];
        Self {
            median: at(50),
            p99: at(99),
            max: samples[samples.len() - 1],
        }
    }
}

/// レコードの大きさ。
#[derive(Debug, Clone, Copy)]
pub struct Size {
    /// 暗号化する前の中身 (更新・スナップショットなど) の大きさ。
    pub payload: usize,
    /// 封筒・暗号文・署名を含むレコード全体の大きさ。
    pub record: usize,
}

impl Size {
    /// 暗号化と署名で増えた分。
    pub fn overhead(&self) -> usize {
        self.record - self.payload
    }
}

/// 本文のログをまとめたスナップショットの大きさ。
#[derive(Debug, Clone, Copy)]
pub struct CompactionSize {
    /// まとめた文書のスナップショット (Loroの出力) の大きさ。
    pub document: usize,
    /// 別に持つ版の時点の中身 (タイトルと本文) の合計。履歴を残すまとめでは0。
    pub contents: usize,
    /// 封筒・暗号文・署名を含むレコード全体の大きさ。
    pub record: usize,
}

/// 計測の結果。
#[derive(Debug)]
pub struct Report {
    pub lines: usize,
    pub runs: usize,
    pub edits: usize,
    /// 計測を始める前の文書のバイト数。
    pub text_bytes: usize,
    /// 計測中の入力をすべて当てた後の文書のバイト数。
    pub final_text_bytes: usize,
    /// 手元の編集1件を、更新の取り出し・著者の署名・暗号化・書き込みの署名まで済ませる時間。
    pub local: Timing,
    /// 届いた更新1件を、中身の検査の規則で確かめて当てるまでの時間。
    pub remote: Timing,
    /// 1文字の入力の更新のレコード。
    pub update: Size,
    /// 文書の全体のスナップショットのレコード。
    pub snapshot: Size,
    /// 版の記録のレコード。中身は版の値の大きさとする。
    pub version: Size,
    /// メンバー4人の世代の記録。中身は、包んだ鍵の合計 (トピックの鍵と、役割に応じた書き込みの鍵の種) とする。
    pub generation: Size,
    /// 版を2つ (計測を始める前と、追加編集の後) 付けた本文のログを、履歴を残してまとめたもの。
    pub compacted_history: CompactionSize,
    /// 同じ本文のログを、履歴を省き、版の時点の中身を別に持ってまとめたもの。
    pub compacted_contents: CompactionSize,
}

impl Report {
    /// 必須の条件 (手元の編集1件と届いた更新1件が、p99で上限以内) を満たしたか。
    pub fn passed(&self) -> bool {
        self.local.p99 <= LIMIT && self.remote.p99 <= LIMIT
    }
}

/// 計測の失敗。
#[derive(Debug, thiserror::Error)]
pub enum MeasureError {
    /// 行数か回数に0を指定した。
    #[error("行数と回数は1以上で指定する")]
    InvalidInput,
    /// 計測用の更新のレコードと、時間の標本を置く容量を確保できなかった。
    #[error("計測用の更新と標本の容量を確保できない")]
    Capacity(#[from] TryReserveError),
    /// 文書の編集や、レコードを封じる・確かめるコアの操作が失敗した。
    #[error("コアの操作が失敗した")]
    Core(#[from] nonoto_core::Error),
    /// 届いた更新のレコードを確かめた結果が、更新ではなかった。
    #[error("届いた更新のレコードを、更新として確かめられなかった")]
    NotUpdate,
    /// 版の記録のレコードを確かめた結果が、版の記録ではなかった。
    #[error("版の記録のレコードを、版の記録として確かめられなかった")]
    NotVersion,
}

/// `lines` 行の文書で、手元の編集と届いた更新を `runs` 回ずつ計り、レコードの大きさを計る。
pub fn run(lines: usize, runs: usize) -> Result<Report, MeasureError> {
    if lines == 0 || runs == 0 {
        return Err(MeasureError::InvalidInput);
    }
    let mut records = Vec::new();
    records.try_reserve_exact(runs)?;
    let mut local_samples = Vec::new();
    local_samples.try_reserve_exact(runs)?;

    let (users, membership) = team()?;
    let author = membership
        .get(&users[0].0)
        .ok_or(nonoto_core::Error::UnknownMember)?;
    let reader = membership
        .get(&users[3].0)
        .ok_or(nonoto_core::Error::UnknownMember)?;
    let topic = TopicId::generate()?;
    let page = SyncPageId::generate()?;
    let keys = GenerationKeys::generate()?;
    let header = RecordHeader {
        kind: RecordKind::Log,
        suite: Suite::CURRENT,
        topic,
        page,
        branch: Branch::Body,
        generation: 1,
    };

    let generation = GenerationRecord::create(
        Suite::CURRENT,
        topic,
        1,
        &keys,
        users[0].0,
        &users[0].1,
        &membership,
    )?;

    let writer = Writer::start(author, &users[0].1)?;
    let dataset = data::japanese_markdown(lines, SEED);
    let mut local = build(&dataset, &writer)?;
    let text_bytes = local.text().len();
    let mut remote = Document::from_snapshot(&local.snapshot()?)?;
    remote.set_writer(Writer::start(reader, &users[3].1)?.id())?;
    let first_version = writer.seal_version(header, &local, keys.topic_key(), keys.body_key())?;

    let update_size = edit_all(
        runs,
        &mut local,
        &writer,
        header,
        &keys,
        &mut records,
        &mut local_samples,
    )?;

    let remote_samples = receive_all(records, &keys, topic, &membership, &mut remote)?;

    let snapshot_header = RecordHeader {
        kind: RecordKind::Snapshot {
            last_sequence: u64::try_from(runs).unwrap_or(u64::MAX),
        },
        ..header
    };
    let snapshot = local.snapshot()?;
    let snapshot_record = SealedRecord::seal(
        snapshot_header,
        snapshot.as_bytes(),
        keys.topic_key(),
        keys.body_key(),
    )?;
    let version = writer.seal_version(header, &local, keys.topic_key(), keys.body_key())?;
    let (compacted_history, compacted_contents) = compactions(
        &writer,
        &local,
        [first_version, version.clone()],
        snapshot_header,
        &keys,
        &membership,
    )?;

    Ok(Report {
        lines: dataset.lines(),
        runs,
        edits: dataset.edits.len(),
        text_bytes,
        final_text_bytes: local.text().len(),
        local: Timing::from_samples(local_samples),
        remote: Timing::from_samples(remote_samples),
        update: update_size,
        snapshot: Size {
            payload: snapshot.as_bytes().len(),
            record: snapshot_record.as_bytes().len(),
        },
        version: Size {
            payload: local.version().as_bytes().len(),
            record: version.as_bytes().len(),
        },
        generation: Size {
            payload: WRAPPED_KEYS,
            record: generation.as_bytes().len(),
        },
        compacted_history,
        compacted_contents,
    })
}

/// 手元の編集1件を `runs` 回当てて封じ、封じたレコードを `records` に、1件ごとの時間を `samples` に入れる。
///
/// 最後の更新の大きさを返す。
fn edit_all(
    runs: usize,
    local: &mut Document,
    writer: &Writer,
    header: RecordHeader,
    keys: &GenerationKeys,
    records: &mut Vec<Vec<u8>>,
    samples: &mut Vec<Duration>,
) -> Result<Size, MeasureError> {
    let mut rng = SeededRng::new(SEED);
    let mut update_size = None;
    for _ in 0..runs {
        let pos = char_boundary(local.text(), rng.below(local.text().len() + 1));
        let started = Instant::now();
        let update = local
            .apply_local(pos..pos, ONE_CHAR)?
            .ok_or(nonoto_core::Error::DocumentEdit)?;
        let record = writer.seal_update(header, &update, keys.topic_key(), keys.body_key())?;
        samples.push(started.elapsed());
        update_size = Some(Size {
            payload: update.as_bytes().len(),
            record: record.as_bytes().len(),
        });
        records.push(record.into_bytes());
    }
    update_size.ok_or(MeasureError::InvalidInput)
}

/// 版の記録を確かめ、本文の文書を版の中身の残し方ごとにまとめて、大きさを返す。
fn compactions(
    writer: &Writer,
    document: &Document,
    version_records: [SealedRecord; 2],
    header: RecordHeader,
    keys: &GenerationKeys,
    membership: &Membership,
) -> Result<(CompactionSize, CompactionSize), MeasureError> {
    let view = GenerationView {
        generation: header.generation,
        write_keys: keys.write_keys(),
        topic_key: keys.topic_key(),
    };
    let author_key = |record_topic: TopicId, member: MemberId| {
        (record_topic == header.topic)
            .then(|| membership.get(&member).map(|member| member.keys.signing))
            .flatten()
    };
    let versions = version_records
        .into_iter()
        .map(
            |record| match check_log_entry(record.into_bytes(), &view, author_key)? {
                CheckedEntry::Version(version) => Ok(version),
                CheckedEntry::Update(_) => Err(MeasureError::NotVersion),
            },
        )
        .collect::<Result<Vec<CheckedVersion>, MeasureError>>()?;
    let compacted = |retention| -> Result<CompactionSize, MeasureError> {
        let record = writer.seal_snapshot(
            header,
            document,
            &versions,
            retention,
            keys.topic_key(),
            keys.body_key(),
        )?;
        let len = record.as_bytes().len();
        let checked = check_snapshot(record.into_bytes(), &view, author_key)?;
        let contents = match retention {
            VersionRetention::History => 0,
            VersionRetention::Contents => checked
                .version_contents()?
                .iter()
                .map(|content| content.title.len() + content.body.len())
                .sum(),
        };
        Ok(CompactionSize {
            document: checked.document_snapshot().as_bytes().len(),
            contents,
            record: len,
        })
    };
    Ok((
        compacted(VersionRetention::History)?,
        compacted(VersionRetention::Contents)?,
    ))
}

/// 届いた更新のレコードを1件ずつ、中身の検査の規則で確かめて当て、1件ごとの時間を返す。
fn receive_all(
    records: Vec<Vec<u8>>,
    keys: &GenerationKeys,
    topic: TopicId,
    membership: &Membership,
    remote: &mut Document,
) -> Result<Vec<Duration>, MeasureError> {
    let view = GenerationView {
        generation: 1,
        write_keys: keys.write_keys(),
        topic_key: keys.topic_key(),
    };
    let mut samples = Vec::new();
    samples.try_reserve_exact(records.len())?;
    for bytes in records {
        let started = Instant::now();
        let checked = check_log_entry(bytes, &view, |record_topic, member| {
            (record_topic == topic)
                .then(|| membership.get(&member).map(|member| member.keys.signing))
                .flatten()
        })?;
        let CheckedEntry::Update(update) = checked else {
            return Err(MeasureError::NotUpdate);
        };
        remote.apply_remote(update.update())?;
        samples.push(started.elapsed());
    }
    Ok(samples)
}

/// 「テストのメンバー構成」の4人 (編集2人・提案1人・閲覧1人) の利用者の鍵とメンバー構成を作る。
fn team() -> Result<(Vec<(MemberId, UserKeys)>, Membership), nonoto_core::Error> {
    let users: Vec<(MemberId, UserKeys)> = (0..4)
        .map(|_| Ok((MemberId::generate()?, UserKeys::generate(Suite::CURRENT)?)))
        .collect::<Result<_, nonoto_core::Error>>()?;
    let roles = [Role::Editor, Role::Editor, Role::Proposer, Role::Viewer];
    let membership = Membership::new(
        users
            .iter()
            .zip(roles)
            .map(|((id, keys), role)| Member {
                id: *id,
                role,
                keys: keys.public_keys().clone(),
            })
            .collect(),
    )?;
    Ok((users, membership))
}

/// 入力の列を、書き手のIDを設定した空の文書に当てて、文書を作る。
fn build(dataset: &Dataset, writer: &Writer) -> Result<Document, nonoto_core::Error> {
    let mut document = Document::new("")?;
    document.set_writer(writer.id())?;
    for edit in &dataset.edits {
        match edit {
            Edit::Insert { pos, text } => document.apply_local(*pos..*pos, text)?,
            Edit::Delete { pos, len } => document.apply_local(*pos..*pos + len, "")?,
        };
    }
    Ok(document)
}

/// `pos` 以下で、最も近い文字の境界を返す。
fn char_boundary(text: &str, mut pos: usize) -> usize {
    while !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 小さい文書で計測を通しで実行できる() {
        let report = run(20, 3).expect("計測できなかった");

        assert_eq!(report.runs, 3, "計った回数が異なる");
        assert!(report.lines >= 20, "文書の行数が足りない: {}", report.lines);
        assert_eq!(
            report.final_text_bytes,
            report.text_bytes + report.runs * ONE_CHAR.len(),
            "計測を始める前の文書と追加編集後の文書のバイト数が合わない",
        );
        for (name, size) in [
            ("更新", report.update),
            ("スナップショット", report.snapshot),
            ("版の記録", report.version),
            ("世代の記録", report.generation),
        ] {
            assert!(
                size.record > size.payload,
                "{name}のレコードが中身より大きくない",
            );
        }
        for (name, size) in [
            ("履歴を残すまとめ", report.compacted_history),
            ("版の中身を別に持つまとめ", report.compacted_contents),
        ] {
            assert!(
                size.record > size.document + size.contents,
                "{name}のレコードが、文書と版の中身の合計より大きくない",
            );
        }
        assert!(
            report.compacted_contents.contents > 0,
            "版の中身を別に持つまとめに、版の中身が無い"
        );
    }

    #[test]
    fn 行数か回数が0なら計測を始めずに拒む() {
        for (lines, runs) in [(0, 1), (1, 0)] {
            assert!(
                matches!(run(lines, runs), Err(MeasureError::InvalidInput)),
                "行数か回数が0の計測を受け付けた",
            );
        }
    }

    #[test]
    fn 標本の容量を確保できなければ通常のエラーを返す() {
        assert!(
            matches!(run(1, usize::MAX), Err(MeasureError::Capacity(_))),
            "容量を確保できない回数の計測を受け付けた",
        );
    }
}
