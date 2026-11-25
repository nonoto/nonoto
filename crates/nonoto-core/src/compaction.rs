//! 本文のログをまとめたスナップショット。編集の役割のメンバーが、取得した最後の順番までをまとめる (ADR #0010)。
//!
//! まとめは、その時点の世代の鍵と方式で暗号化し、本文の書き込みの鍵で署名する。
//! 中継はまとめた順番の上限までのレコードを消すため、本文のログにあった版の記録は、スナップショットの中に移す。
//! スナップショットの中身は、次のバイト列にする。
//!
//! ```text
//! 版の中身の残し方 (1) | まとめたメンバーのID (16) | 文書の長さ (4) | 文書 | 版の数 (4)
//!   | 版ごとに { 版の記録の長さ (4) | 版の記録 | 残し方が「中身」なら { タイトルの長さ (4) | タイトル | 本文の長さ (4) | 本文 } }
//!   | まとめたメンバーの署名 (64)
//! ```
//!
//! 署名の対象は、区切りの文字列・トピックのID・ページのID・まとめた順番の上限 (8)・署名より前の中身である。
//! 版の記録は、版を作ったメンバーの署名ごと入れるため、受け手は作ったメンバーの署名を確かめ直せる。

use std::fmt;

use zeroize::Zeroizing;

use crate::author::Writer;
use crate::crypto::{SIGNATURE_LEN, TopicKey, UserVerifyingKey, WriteSigningKey};
use crate::document::{Document, Snapshot, VersionContent};
use crate::error::Error;
use crate::member::MemberId;
use crate::receive::{CheckedVersion, GenerationView};
use crate::record::{Branch, ID_LEN, Reader, RecordHeader, RecordKind, SealedRecord, TopicId};
use crate::version::VersionEntry;

/// まとめたメンバーの署名の、用途の区切りの文字列。
const COMPACTOR_SIGNATURE_DOMAIN: &[u8] = b"nonoto/snapshot/signature\0";

const RETENTION_HISTORY: u8 = 1;
const RETENTION_CONTENTS: u8 = 2;

/// まとめたあとも版の中身を取り出せるようにする、版の中身の残し方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRetention {
    /// 文書の履歴をすべて残し、版の中身は文書から取り出す。
    ///
    /// まとめより前から分かれた端末の更新も、まとめた文書に当てられる。
    History,
    /// 履歴を今の時点より前で省き、版ごとの時点の中身 (タイトルと本文) を別に持つ。
    ///
    /// 省いた文書は、まとめより前から分かれた更新を当てられないため、同期の処理は使わず、大きさを比べるのに使う。
    Contents,
}

impl VersionRetention {
    fn value(self) -> u8 {
        match self {
            Self::History => RETENTION_HISTORY,
            Self::Contents => RETENTION_CONTENTS,
        }
    }

    fn from_value(value: u8) -> Result<Self, Error> {
        match value {
            RETENTION_HISTORY => Ok(Self::History),
            RETENTION_CONTENTS => Ok(Self::Contents),
            _ => Err(Error::MalformedRecord),
        }
    }
}

impl Writer {
    /// 本文の文書と、本文のログにあった版の記録を、スナップショットとして封じる。
    ///
    /// 封筒は本文のスナップショットにし、まとめた順番の上限を置く。そうでなければ [`Error::UnexpectedRecordKind`] を返す。
    /// 版は、文書がその版までの操作を持ち、中身がハッシュと合うものだけを渡す。
    /// 合わなければ [`Error::VersionNotFound`] か [`Error::VersionContentMismatch`] を返す。
    /// 版のトピックかページが封筒と異なる場合は [`Error::ResealTargetMismatch`] を返す。
    /// 書き込みの鍵には本文の書き込みの鍵を渡す。
    pub fn seal_snapshot(
        &self,
        header: RecordHeader,
        document: &Document,
        versions: &[CheckedVersion],
        retention: VersionRetention,
        topic_key: &TopicKey,
        write_key: &WriteSigningKey,
    ) -> Result<SealedRecord, Error> {
        let RecordKind::Snapshot { last_sequence } = header.kind else {
            return Err(Error::UnexpectedRecordKind);
        };
        if header.branch != Branch::Body {
            return Err(Error::UnexpectedRecordKind);
        }
        let snapshot = match retention {
            VersionRetention::History => document.snapshot()?,
            VersionRetention::Contents => document.shallow_snapshot()?,
        };
        let mut payload = Zeroizing::new(Vec::new());
        payload.push(retention.value());
        payload.extend_from_slice(self.member().as_bytes());
        push_bytes(&mut payload, snapshot.as_bytes())?;
        push_len(&mut payload, versions.len())?;
        for version in versions {
            if version.header().topic != header.topic || version.header().page != header.page {
                return Err(Error::ResealTargetMismatch);
            }
            let content = version.content(document)?;
            push_bytes(&mut payload, &version.entry().encode()?)?;
            if retention == VersionRetention::Contents {
                push_bytes(&mut payload, content.title.as_bytes())?;
                push_bytes(&mut payload, content.body.as_bytes())?;
            }
        }
        let message = signed_message(&header, last_sequence, &payload);
        payload.extend_from_slice(&self.signing_key().sign(&message));
        SealedRecord::seal(header, &payload, topic_key, write_key)
    }
}

/// 署名と版の記録を確かめた、まとめたスナップショット。
#[derive(Clone, PartialEq, Eq)]
pub struct CheckedSnapshot {
    header: RecordHeader,
    last_sequence: u64,
    compactor: MemberId,
    retention: VersionRetention,
    document: Snapshot,
    versions: Vec<CheckedVersion>,
    /// 残し方が「中身」のときの、版ごとの時点の中身。版と同じ順に並べる。
    contents: Vec<VersionContent>,
}

impl fmt::Debug for CheckedSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckedSnapshot")
            .field("header", &self.header)
            .field("retention", &self.retention)
            .field("version_count", &self.versions.len())
            .finish_non_exhaustive()
    }
}

impl CheckedSnapshot {
    /// スナップショットの封筒を返す。
    pub fn header(&self) -> &RecordHeader {
        &self.header
    }

    /// まとめた範囲の、ログの順番の上限。
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// まとめたメンバー。
    pub fn compactor(&self) -> MemberId {
        self.compactor
    }

    /// 版の中身の残し方。
    pub fn retention(&self) -> VersionRetention {
        self.retention
    }

    /// まとめた範囲にあった版の記録。作ったメンバーの署名は確かめてある。
    pub fn versions(&self) -> &[CheckedVersion] {
        &self.versions
    }

    /// まとめた文書のスナップショット。履歴を残したまとめなら、手元の文書に [`Document::apply_snapshot`] で合わせられる。
    pub fn document_snapshot(&self) -> &Snapshot {
        &self.document
    }

    /// まとめた文書を作る。履歴を省いたまとめ ([`VersionRetention::Contents`]) なら [`Error::TrimmedSnapshot`] を返す。
    pub fn document(&self) -> Result<Document, Error> {
        match self.retention {
            VersionRetention::History => Document::from_snapshot(&self.document),
            VersionRetention::Contents => Err(Error::TrimmedSnapshot),
        }
    }

    /// 版ごとの時点の中身を、版と同じ順に取り出す。どれも版の記録の中身のハッシュと照らしてある。
    ///
    /// 履歴を残したまとめでは文書から取り出し、文書がその版までの操作を持っていなければ [`Error::VersionNotFound`] を返す。
    pub fn version_contents(&self) -> Result<Vec<VersionContent>, Error> {
        match self.retention {
            VersionRetention::History => {
                let document = self.document()?;
                self.versions
                    .iter()
                    .map(|version| version.content(&document))
                    .collect()
            }
            VersionRetention::Contents => Ok(self.contents.clone()),
        }
    }
}

/// 受け取ったスナップショットを、次の順に確かめる。
///
/// 1. 封筒を読む。知らない封筒の版・方式のIDなら止める。本文のスナップショットでなければ [`Error::UnexpectedRecordKind`]、
///    渡した世代と違えば [`Error::UnknownGeneration`] を返す
/// 2. 書き込みの署名を、本文の書き込みの公開鍵で確かめる
/// 3. 復号する。知らない中身の版なら止める
/// 4. まとめたメンバーの署名を、そのメンバーの利用者の公開鍵で確かめる ([`Error::InvalidCompactorSignature`])。
///    中の更新ごとの著者は確かめない (ADR #0009)
/// 5. 版の記録ごとに、作ったメンバーの署名と版の値の形式を確かめる。中身を別に持つまとめでは、中身をハッシュと照らす
///
/// `author_key` は、トピックIDとメンバーIDに対応する認証済みの署名の公開鍵を返す。無ければ [`Error::UnknownMember`] を返す。
pub fn check_snapshot(
    bytes: Vec<u8>,
    keys: &GenerationView<'_>,
    mut author_key: impl FnMut(TopicId, MemberId) -> Option<UserVerifyingKey>,
) -> Result<CheckedSnapshot, Error> {
    let record = SealedRecord::parse(bytes)?;
    let header = *record.header();
    let RecordKind::Snapshot { last_sequence } = header.kind else {
        return Err(Error::UnexpectedRecordKind);
    };
    if header.branch != Branch::Body {
        return Err(Error::UnexpectedRecordKind);
    }
    if header.generation != keys.generation {
        return Err(Error::UnknownGeneration(header.generation));
    }
    let record = record.verify(&keys.write_keys.body)?;
    let payload = record.open(keys.topic_key)?;
    let payload = payload.as_bytes();

    let (signed, signature) = payload
        .split_last_chunk::<SIGNATURE_LEN>()
        .ok_or(Error::MalformedRecord)?;
    let mut reader = Reader::new(signed);
    let retention = VersionRetention::from_value(reader.u8()?)?;
    let compactor = MemberId::from_bytes(reader.array::<ID_LEN>()?);
    let compactor_key = author_key(header.topic, compactor).ok_or(Error::UnknownMember)?;
    compactor_key
        .verify_author(&signed_message(&header, last_sequence, signed), signature)
        .map_err(|_| Error::InvalidCompactorSignature)?;

    let document = Snapshot::from_bytes(take_bytes(&mut reader)?.to_vec());
    let count = take_len(&mut reader)?;
    let mut versions = Vec::new();
    let mut contents = Vec::new();
    for _ in 0..count {
        let entry = VersionEntry::decode(take_bytes(&mut reader)?)?;
        let creator_key = author_key(header.topic, entry.creator).ok_or(Error::UnknownMember)?;
        entry.verify(&header.topic, &header.page, &creator_key)?;
        entry.version.validate()?;
        let version = CheckedVersion::from_entry(header, entry);
        if retention == VersionRetention::Contents {
            let title = take_text(&mut reader)?;
            let body = take_text(&mut reader)?;
            version.check_content_hash(&title, &body)?;
            contents.push(VersionContent { title, body });
        }
        versions.push(version);
    }
    if !reader.remaining().is_empty() {
        return Err(Error::MalformedRecord);
    }
    Ok(CheckedSnapshot {
        header,
        last_sequence,
        compactor,
        retention,
        document,
        versions,
        contents,
    })
}

fn signed_message(header: &RecordHeader, last_sequence: u64, payload: &[u8]) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(
        [
            COMPACTOR_SIGNATURE_DOMAIN,
            header.topic.as_bytes(),
            header.page.as_bytes(),
            &last_sequence.to_be_bytes(),
            payload,
        ]
        .concat(),
    )
}

fn push_len(bytes: &mut Vec<u8>, len: usize) -> Result<(), Error> {
    let len = u32::try_from(len).map_err(|_| Error::RecordTooLarge)?;
    bytes.extend_from_slice(&len.to_be_bytes());
    Ok(())
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), Error> {
    push_len(bytes, value.len())?;
    bytes.extend_from_slice(value);
    Ok(())
}

fn take_len(reader: &mut Reader<'_>) -> Result<usize, Error> {
    usize::try_from(reader.u32()?).map_err(|_| Error::MalformedRecord)
}

fn take_bytes<'a>(reader: &mut Reader<'a>) -> Result<&'a [u8], Error> {
    let len = take_len(reader)?;
    reader.take(len)
}

fn take_text(reader: &mut Reader<'_>) -> Result<String, Error> {
    String::from_utf8(take_bytes(reader)?.to_vec()).map_err(|_| Error::MalformedRecord)
}

#[cfg(test)]
mod tests {
    use loro::LoroDoc;

    use super::*;
    use crate::crypto::Suite;
    use crate::generation::GenerationKeys;
    use crate::member::{Member, Role, UserKeys};
    use crate::receive::{CheckedEntry, check_log_entry};
    use crate::record::SyncPageId;

    const FIRST: &str = "一版目。";
    const LATER: &str = "二版目の追記。";

    struct Fixture {
        member: Member,
        keys: UserKeys,
        other: UserKeys,
        generation: GenerationKeys,
        topic: TopicId,
        page: SyncPageId,
    }

    impl Fixture {
        fn new() -> Self {
            let keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
            Self {
                member: Member {
                    id: MemberId::generate().expect("メンバーのIDを作れなかった"),
                    role: Role::Editor,
                    keys: keys.public_keys().clone(),
                },
                keys,
                other: UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった"),
                generation: GenerationKeys::generate().expect("世代の鍵を作れなかった"),
                topic: TopicId::generate().expect("トピックのIDを作れなかった"),
                page: SyncPageId::generate().expect("ページのIDを作れなかった"),
            }
        }

        fn writer(&self) -> Writer {
            Writer::start(&self.member, &self.keys).expect("書き手を作れなかった")
        }

        fn header(&self, kind: RecordKind) -> RecordHeader {
            RecordHeader {
                kind,
                suite: Suite::CURRENT,
                topic: self.topic,
                page: self.page,
                branch: Branch::Body,
                generation: 1,
            }
        }

        fn view(&self) -> GenerationView<'_> {
            GenerationView {
                generation: 1,
                write_keys: self.generation.write_keys(),
                topic_key: self.generation.topic_key(),
            }
        }

        fn author_key(&self, _topic: TopicId, member: MemberId) -> Option<UserVerifyingKey> {
            (member == self.member.id).then_some(self.member.keys.signing)
        }

        /// 1つ目の本文に版を付け、本文を書き足した文書と、確かめた版の記録を返す。
        fn versioned(&self, writer: &Writer) -> (Document, Vec<CheckedVersion>) {
            let mut document = Document::new("").expect("文書を作れなかった");
            document
                .set_writer(writer.id())
                .expect("書き手を設定できなかった");
            document
                .apply_local(0..0, FIRST)
                .expect("文書を書けなかった");
            let record = writer
                .seal_version(
                    self.header(RecordKind::Log),
                    &document,
                    self.generation.topic_key(),
                    self.generation.body_key(),
                )
                .expect("版の記録を封じられなかった");
            let CheckedEntry::Version(version) =
                check_log_entry(record.into_bytes(), &self.view(), |topic, member| {
                    self.author_key(topic, member)
                })
                .expect("版の記録を確かめられなかった")
            else {
                panic!("版の記録が更新として確かめられた");
            };
            let end = document.text().len();
            document
                .apply_local(end..end, LATER)
                .expect("文書を書けなかった");
            (document, vec![version])
        }

        fn seal(
            &self,
            writer: &Writer,
            document: &Document,
            versions: &[CheckedVersion],
            retention: VersionRetention,
        ) -> Result<SealedRecord, Error> {
            writer.seal_snapshot(
                self.header(RecordKind::Snapshot { last_sequence: 7 }),
                document,
                versions,
                retention,
                self.generation.topic_key(),
                self.generation.body_key(),
            )
        }

        fn check(&self, record: SealedRecord) -> Result<CheckedSnapshot, Error> {
            check_snapshot(record.into_bytes(), &self.view(), |topic, member| {
                self.author_key(topic, member)
            })
        }
    }

    #[test]
    fn 履歴を残すまとめと中身を別に持つまとめのどちらからも_版の中身を取り出せる() {
        let fixture = Fixture::new();
        let writer = fixture.writer();
        let (document, versions) = fixture.versioned(&writer);

        for retention in [VersionRetention::History, VersionRetention::Contents] {
            let record = fixture
                .seal(&writer, &document, &versions, retention)
                .expect("まとめを封じられなかった");
            let checked = fixture.check(record).expect("まとめを確かめられなかった");

            assert_eq!(checked.retention(), retention, "版の中身の残し方が異なる");
            assert_eq!(checked.last_sequence(), 7, "まとめた順番の上限が異なる");
            assert_eq!(
                checked.compactor(),
                fixture.member.id,
                "まとめたメンバーが異なる"
            );
            assert_eq!(checked.versions().len(), 1, "まとめた版の数が異なる");
            let contents = checked
                .version_contents()
                .expect("まとめから版の中身を取り出せなかった");
            assert_eq!(contents.len(), 1, "取り出した版の中身の数が異なる");
            assert_eq!(contents[0].body, FIRST, "版の時点の本文が異なる");
            match retention {
                VersionRetention::History => assert_eq!(
                    checked
                        .document()
                        .expect("まとめから文書を作れなかった")
                        .text(),
                    document.text(),
                    "まとめた文書の本文が異なる",
                ),
                VersionRetention::Contents => assert!(
                    matches!(checked.document(), Err(Error::TrimmedSnapshot)),
                    "履歴を省いたまとめから文書を作った",
                ),
            }
        }
    }

    #[test]
    fn 履歴を省いた文書は_まとめより前から分かれた更新を当てられない() {
        let fixture = Fixture::new();
        let mut compactor = Document::new("").expect("文書を作れなかった");
        compactor
            .set_writer(fixture.writer().id())
            .expect("書き手を設定できなかった");
        compactor
            .apply_local(0..0, FIRST)
            .expect("文書を書けなかった");
        let mut offline = Document::from_snapshot(
            &compactor
                .snapshot()
                .expect("スナップショットを出力できなかった"),
        )
        .expect("スナップショットから文書を作れなかった");
        offline
            .set_writer(fixture.writer().id())
            .expect("書き手を設定できなかった");
        let update = offline
            .apply_local(0..0, "切れているあいだ。")
            .expect("文書を書けなかった")
            .expect("更新が無い");
        let end = compactor.text().len();
        compactor
            .apply_local(end..end, LATER)
            .expect("文書を書けなかった");

        let mut full = Document::from_snapshot(
            &compactor
                .snapshot()
                .expect("スナップショットを出力できなかった"),
        )
        .expect("スナップショットから文書を作れなかった");
        full.apply_remote(&update)
            .expect("履歴を残した文書に、まとめより前から分かれた更新を当てられなかった");
        let trimmed = LoroDoc::from_snapshot(
            compactor
                .shallow_snapshot()
                .expect("履歴を省いたスナップショットを出力できなかった")
                .as_bytes(),
        )
        .expect("履歴を省いたスナップショットを読めなかった");
        assert!(
            trimmed.import(update.as_bytes()).is_err(),
            "履歴を省いた文書に、まとめより前から分かれた更新を当てた",
        );
    }

    #[test]
    fn まとめたメンバーの署名が合わないか_分からないまとめを拒む() {
        let fixture = Fixture::new();
        let writer = fixture.writer();
        let (document, versions) = fixture.versioned(&writer);
        let record = fixture
            .seal(&writer, &document, &versions, VersionRetention::History)
            .expect("まとめを封じられなかった");

        assert!(
            matches!(
                check_snapshot(record.as_bytes().to_vec(), &fixture.view(), |_, _| Some(
                    fixture.other.public_keys().signing
                )),
                Err(Error::InvalidCompactorSignature)
            ),
            "別の利用者の公開鍵で、まとめたメンバーの署名を受け付けた",
        );
        assert!(
            matches!(
                check_snapshot(record.into_bytes(), &fixture.view(), |_, _| None),
                Err(Error::UnknownMember)
            ),
            "公開鍵の分からないメンバーのまとめを受け付けた",
        );
    }

    #[test]
    fn 本文のスナップショット以外の封筒と_文書に無い版ではまとめない() {
        let fixture = Fixture::new();
        let writer = fixture.writer();
        let (document, versions) = fixture.versioned(&writer);

        assert!(
            matches!(
                writer.seal_snapshot(
                    fixture.header(RecordKind::Log),
                    &document,
                    &versions,
                    VersionRetention::History,
                    fixture.generation.topic_key(),
                    fixture.generation.body_key(),
                ),
                Err(Error::UnexpectedRecordKind)
            ),
            "ログのレコードの封筒でまとめを封じた",
        );
        let unrelated = Document::new("別の文書。").expect("文書を作れなかった");
        assert!(
            matches!(
                fixture.seal(&writer, &unrelated, &versions, VersionRetention::History),
                Err(Error::VersionNotFound)
            ),
            "文書に無い版をまとめに入れた",
        );
        let log = writer
            .seal_version(
                fixture.header(RecordKind::Log),
                &document,
                fixture.generation.topic_key(),
                fixture.generation.body_key(),
            )
            .expect("版の記録を封じられなかった");
        assert!(
            matches!(fixture.check(log), Err(Error::UnexpectedRecordKind)),
            "ログのレコードをまとめとして受け付けた",
        );
    }

    #[test]
    fn 文書が同じでも別ページの版をまとめに含めない() {
        let fixture = Fixture::new();
        let writer = fixture.writer();
        let (document, versions) = fixture.versioned(&writer);
        let header = RecordHeader {
            page: SyncPageId::generate().expect("別ページのIDを作れなかった"),
            ..fixture.header(RecordKind::Snapshot { last_sequence: 7 })
        };
        assert!(
            matches!(
                writer.seal_snapshot(
                    header,
                    &document,
                    &versions,
                    VersionRetention::History,
                    fixture.generation.topic_key(),
                    fixture.generation.body_key(),
                ),
                Err(Error::ResealTargetMismatch)
            ),
            "別ページに署名された版をまとめに含めた"
        );
    }

    #[test]
    fn 確かめたまとめをデバッグ表示しても本文を出さない() {
        let fixture = Fixture::new();
        let writer = fixture.writer();
        let (document, versions) = fixture.versioned(&writer);
        let record = fixture
            .seal(&writer, &document, &versions, VersionRetention::Contents)
            .expect("まとめを封じられなかった");
        let shown = format!(
            "{:?}",
            fixture.check(record).expect("まとめを確かめられなかった")
        );
        assert!(
            !shown.contains(FIRST) && !shown.contains(LATER),
            "デバッグ表示に本文が出た: {shown}"
        );
    }
}
