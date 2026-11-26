//! 受け取ったログのレコードを、「中身の検査の規則」で確かめる。
//!
//! 規則は、レコードそのもの・世代の書き込みの公開鍵・メンバーの利用者の公開鍵だけで決まり、
//! 届いた順やその時点のメンバー構成の変化によらないため、どの端末でも同じ結果になる。
//! 1つでも合わなければエラーを返し、呼び出し側は文書に当てない。

use std::fmt;

use crate::author::{AuthoredUpdate, ENTRY_UPDATE, WriterId, WriterNonce};
use crate::crypto::{SIGNATURE_LEN, TopicKey, UserVerifyingKey, WriteSigningKey};
use crate::document::{Document, Update, Version, VersionContent, check_update_writer};
use crate::error::Error;
use crate::generation::WriteKeys;
use crate::member::MemberId;
use crate::record::{Branch, RecordHeader, RecordKind, SealedRecord, TopicId, WriteKeyRole};
use crate::version::{ContentHash, ENTRY_VERSION, VersionEntry};

/// レコードを確かめるのに使う、1つの世代の鍵。
#[derive(Debug, Clone, Copy)]
pub struct GenerationView<'a> {
    /// 世代の番号。
    pub generation: u64,
    /// 世代の記録の封筒にある書き込みの公開鍵。
    pub write_keys: WriteKeys,
    /// 包みから受け取ったトピックの鍵。
    pub topic_key: &'a TopicKey,
}

/// 規則をすべて満たした更新。
#[derive(Clone, PartialEq, Eq)]
pub struct CheckedUpdate {
    header: RecordHeader,
    author: MemberId,
    nonce: WriterNonce,
    writer: WriterId,
    update: Update,
    signature: [u8; SIGNATURE_LEN],
}

impl fmt::Debug for CheckedUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckedUpdate")
            .field("header", &self.header)
            .field("update", &self.update)
            .finish_non_exhaustive()
    }
}

impl CheckedUpdate {
    /// 確かめた封筒の情報を返す。
    pub fn header(&self) -> &RecordHeader {
        &self.header
    }

    /// 著者のメンバー。
    pub fn author(&self) -> MemberId {
        self.author
    }

    /// 更新の書き手のID。著者の公開鍵と書き始めの乱数から導いた値と一致する。
    pub fn writer(&self) -> WriterId {
        self.writer
    }

    /// 文書に当てる更新。
    pub fn update(&self) -> &Update {
        &self.update
    }

    /// 著者の署名をそのまま使い、別の封筒 (書く先の枝・世代) のログのレコードとして封じ直す。
    ///
    /// 承認で編集提案の枝の更新を本文のログに書き直すときと、古い世代の更新を今の世代で暗号化し直すときに使う。
    /// 著者の署名は枝と世代を含まないため、封じ直しても受け手は同じ著者の署名として確かめられる。
    /// 封筒がログのレコードでなければ [`Error::UnexpectedRecordKind`]、
    /// 元と別のトピックかページを指していれば [`Error::ResealTargetMismatch`] を返す。
    pub fn reseal(
        &self,
        header: RecordHeader,
        topic_key: &TopicKey,
        write_key: &WriteSigningKey,
    ) -> Result<SealedRecord, Error> {
        if header.kind != RecordKind::Log {
            return Err(Error::UnexpectedRecordKind);
        }
        if header.topic != self.header.topic || header.page != self.header.page {
            return Err(Error::ResealTargetMismatch);
        }
        let entry = AuthoredUpdate::from_parts(
            self.author,
            self.nonce,
            self.update.as_bytes(),
            self.signature,
        );
        SealedRecord::seal(header, &entry.encode()?, topic_key, write_key)
    }
}

/// 署名と版の値の形式を確かめた版の記録。
///
/// 文書の中身との照合は [`CheckedVersion::content`] や [`CheckedVersion::fork`] で行う。
#[derive(Clone, PartialEq, Eq)]
pub struct CheckedVersion {
    header: RecordHeader,
    entry: VersionEntry,
}

impl fmt::Debug for CheckedVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckedVersion")
            .field("header", &self.header)
            .finish_non_exhaustive()
    }
}

impl CheckedVersion {
    /// 署名と版の値の形式を確かめた版の記録の中身を、運んだ封筒と組にする。
    pub(crate) fn from_entry(header: RecordHeader, entry: VersionEntry) -> Self {
        Self { header, entry }
    }

    /// 版の記録を運んだ封筒を返す。まとめたスナップショットから取り出した版では、スナップショットの封筒を返す。
    pub fn header(&self) -> &RecordHeader {
        &self.header
    }

    /// 版を作ったメンバー。
    pub fn creator(&self) -> MemberId {
        self.entry.creator
    }

    /// 版が指す時点。
    pub fn version(&self) -> &Version {
        &self.entry.version
    }

    /// 署名で確かめた、版の時点のタイトルと本文のハッシュを借りる。
    pub fn content_hash(&self) -> &ContentHash {
        &self.entry.content_hash
    }

    /// 作ったメンバーの署名を含む、版の記録の中身を借りる。まとめたスナップショットに入れ直すのに使う。
    pub(crate) fn entry(&self) -> &VersionEntry {
        &self.entry
    }

    /// 文書から版の時点の中身を取り出し、版の記録の中身のハッシュと照らして返す。
    ///
    /// 文書がその版までの操作を持っていなければ [`Error::VersionNotFound`]、
    /// 中身がハッシュと合わなければ [`Error::VersionContentMismatch`] を返す。
    pub fn content(&self, document: &Document) -> Result<VersionContent, Error> {
        let content = document.content_at(&self.entry.version)?;
        self.check_content_hash(&content.title, &content.body)?;
        Ok(content)
    }

    /// 署名したハッシュと中身が合うことを確かめ、受信した版を基準に編集提案の枝を開く。
    ///
    /// 版までの操作がまだ無ければ [`Error::VersionNotFound`]、
    /// 中身が合わなければ [`Error::VersionContentMismatch`] を返す。
    /// 枝に書く前に [`Document::set_writer`] で書き手のIDを設定する。
    pub fn fork(&self, document: &Document) -> Result<Document, Error> {
        let branch = document.fork_at(&self.entry.version)?;
        self.check_content_hash(branch.title(), branch.text())?;
        Ok(branch)
    }

    pub(crate) fn check_content_hash(&self, title: &str, body: &str) -> Result<(), Error> {
        if ContentHash::of_text(title, body) != self.entry.content_hash {
            return Err(Error::VersionContentMismatch);
        }
        Ok(())
    }
}

/// [`check_log_entry`] が確かめたログのレコードの中身。
///
/// 本文の更新と版の記録は、サーバーから見分けられない同じ種類のレコードで届き、復号してから見分ける。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedEntry {
    /// 本文か編集提案の枝に当てる更新。
    Update(CheckedUpdate),
    /// 本文にある時点を指す版の記録。文書の中身とは取り出す際に照合する。
    Version(CheckedVersion),
}

/// 受け取ったログのレコードを、更新の記録として確かめる。
///
/// 確かめる順は [`check_log_entry`] と同じで、版の記録なら [`Error::UnexpectedEntryKind`] を返す。
pub fn check_log_record(
    bytes: Vec<u8>,
    keys: &GenerationView<'_>,
    author_key: impl FnOnce(TopicId, MemberId) -> Option<UserVerifyingKey>,
) -> Result<CheckedUpdate, Error> {
    match check_log_entry(bytes, keys, author_key)? {
        CheckedEntry::Update(update) => Ok(update),
        CheckedEntry::Version(_) => Err(Error::UnexpectedEntryKind),
    }
}

/// 受け取ったログのレコードを、次の順に確かめる。
///
/// `author_key` は、トピックIDとメンバーIDに対応する認証済みの署名の公開鍵を返す。
/// 元メンバーの鍵も照合できるようにし、同じログを受け取った端末が同じ公開鍵で確かめる。
///
/// 1. 封筒を読む。知らない封筒の版・方式のIDなら止める。ログのレコードでなければ [`Error::UnexpectedRecordKind`]、
///    渡した世代と違えば [`Error::UnknownGeneration`] を返す
/// 2. 書き込みの署名を、書く先 (本文か編集提案) の書き込みの公開鍵で確かめる
/// 3. 用途とページのIDから導いた鍵で復号する。知らない中身の版なら止める
///
/// 更新の記録は、続けて次を確かめる。
///
/// 4. 著者の署名を、著者のメンバーの利用者の公開鍵で確かめる。著者の認証済みの公開鍵が無ければ [`Error::UnknownMember`] を返す
/// 5. 更新の書き手のIDが、著者の公開鍵と中身の乱数から導いた値と一致することを確かめる ([`Error::WriterIdMismatch`])
/// 6. 更新が、その書き手のIDの操作だけを含むことを確かめる ([`Error::ForeignOperations`])
///
/// 版の記録は、本文のログにあること、作ったメンバーの署名、版の値の形式を確かめる (7)。
/// 文書がその版までの操作を持ってから、[`CheckedVersion::content`] や [`CheckedVersion::fork`] で中身を照合する。
pub fn check_log_entry(
    bytes: Vec<u8>,
    keys: &GenerationView<'_>,
    author_key: impl FnOnce(TopicId, MemberId) -> Option<UserVerifyingKey>,
) -> Result<CheckedEntry, Error> {
    let record = SealedRecord::parse(bytes)?;
    let header = *record.header();
    if header.kind != RecordKind::Log {
        return Err(Error::UnexpectedRecordKind);
    }
    if header.generation != keys.generation {
        return Err(Error::UnknownGeneration(header.generation));
    }

    let write_key = match header.write_key_role() {
        WriteKeyRole::Body => keys.write_keys.body,
        WriteKeyRole::Proposal => keys.write_keys.proposal,
    };
    let record = record.verify(&write_key)?;
    let payload = record.open(keys.topic_key)?;

    match payload.as_bytes().first() {
        Some(&ENTRY_UPDATE) => {
            check_update(header, payload.as_bytes(), author_key).map(CheckedEntry::Update)
        }
        Some(&ENTRY_VERSION) => {
            check_version(header, payload.as_bytes(), author_key).map(CheckedEntry::Version)
        }
        _ => Err(Error::MalformedRecord),
    }
}

fn check_update(
    header: RecordHeader,
    payload: &[u8],
    author_key: impl FnOnce(TopicId, MemberId) -> Option<UserVerifyingKey>,
) -> Result<CheckedUpdate, Error> {
    let entry = AuthoredUpdate::decode(payload)?;
    let author_key = author_key(header.topic, entry.author).ok_or(Error::UnknownMember)?;
    entry.verify(&header.topic, &header.page, &author_key)?;

    let writer = WriterId::derive(&author_key, &entry.nonce);
    check_update_writer(entry.update, writer)?;

    Ok(CheckedUpdate {
        header,
        author: entry.author,
        nonce: entry.nonce,
        writer,
        update: Update::from_bytes(entry.update.to_vec()),
        signature: entry.signature(),
    })
}

fn check_version(
    header: RecordHeader,
    payload: &[u8],
    creator_key: impl FnOnce(TopicId, MemberId) -> Option<UserVerifyingKey>,
) -> Result<CheckedVersion, Error> {
    // 版は本文のある時点に付けるため、編集提案の枝のログに置いた版の記録は受け付けない。
    if header.branch != Branch::Body {
        return Err(Error::UnexpectedEntryKind);
    }
    let entry = VersionEntry::decode(payload)?;
    let creator_key = creator_key(header.topic, entry.creator).ok_or(Error::UnknownMember)?;
    entry.verify(&header.topic, &header.page, &creator_key)?;
    entry.version.validate()?;
    Ok(CheckedVersion { header, entry })
}

#[cfg(test)]
mod tests {
    use loro::{ExportMode, LoroDoc};

    use super::*;
    use crate::author::{Writer, WriterNonce};
    use crate::crypto::{Suite, WriteSigningKey};
    use crate::document::{Document, Snapshot};
    use crate::generation::GenerationKeys;
    use crate::member::{Member, Membership, Role, UserKeys};
    use crate::record::{Branch, BranchId, SyncPageId};

    /// 「テストのメンバー構成」のA (編集。2台)・B (編集)・C (提案)・D (閲覧)。
    struct Fixture {
        users: Vec<(MemberId, UserKeys)>,
        membership: Membership,
        keys: GenerationKeys,
        topic: TopicId,
        page: SyncPageId,
        base: Snapshot,
    }

    const A: usize = 0;
    const B: usize = 1;
    const C: usize = 2;
    const D: usize = 3;

    impl Fixture {
        fn new() -> Self {
            let roles = [Role::Editor, Role::Editor, Role::Proposer, Role::Viewer];
            let users: Vec<(MemberId, UserKeys)> = roles
                .iter()
                .map(|_| {
                    (
                        MemberId::generate().expect("メンバーのIDを作れなかった"),
                        UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった"),
                    )
                })
                .collect();
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
            )
            .expect("メンバー構成を作れなかった");
            let base = Document::new("")
                .and_then(|document| document.snapshot())
                .expect("最初の文書を作れなかった");
            Self {
                users,
                membership,
                keys: GenerationKeys::generate().expect("世代の鍵を作れなかった"),
                topic: TopicId::generate().expect("トピックのIDを作れなかった"),
                page: SyncPageId::generate().expect("ページのIDを作れなかった"),
                base,
            }
        }

        fn view(&self) -> GenerationView<'_> {
            GenerationView {
                generation: 1,
                write_keys: self.keys.write_keys(),
                topic_key: self.keys.topic_key(),
            }
        }

        fn header(&self, branch: Branch) -> RecordHeader {
            RecordHeader {
                kind: RecordKind::Log,
                suite: Suite::CURRENT,
                topic: self.topic,
                page: self.page,
                branch,
                generation: 1,
            }
        }

        /// メンバーが書き始めた文書を開く。
        fn open(&self, index: usize) -> (Writer, Document) {
            let (id, keys) = &self.users[index];
            let member = self.membership.get(id).expect("メンバー構成にいない");
            let writer = Writer::start(member, keys).expect("書き始められなかった");
            let mut document = Document::from_snapshot(&self.base).expect("文書を開けなかった");
            document
                .set_writer(writer.id())
                .expect("書き手のIDを設定できなかった");
            (writer, document)
        }

        fn seal(&self, writer: &Writer, update: &Update, key: &WriteSigningKey) -> Vec<u8> {
            writer
                .seal_update(
                    self.header(Branch::Body),
                    update,
                    self.keys.topic_key(),
                    key,
                )
                .expect("レコードを封じられなかった")
                .into_bytes()
        }

        /// レコードを確かめ、合えば文書に当てる。
        fn receive(&self, document: &mut Document, bytes: Vec<u8>) -> Result<(), Error> {
            let checked = self.check(bytes)?;
            document.apply_remote(checked.update()).map(|_| ())
        }

        fn check(&self, bytes: Vec<u8>) -> Result<CheckedUpdate, Error> {
            check_log_record(bytes, &self.view(), |topic, author| {
                if topic != self.topic {
                    return None;
                }
                self.membership
                    .get(&author)
                    .map(|member| member.keys.signing)
            })
        }

        /// 著者と乱数を指定し、書き手のIDを照合せずに更新を封じる。
        fn seal_entry(&self, author: usize, nonce: WriterNonce, update: &Update) -> Vec<u8> {
            let (member, keys) = &self.users[author];
            let entry = AuthoredUpdate::sign(
                self.topic,
                self.page,
                *member,
                keys.signing(),
                nonce,
                update.as_bytes(),
            );
            SealedRecord::seal(
                self.header(Branch::Body),
                &entry.encode().expect("更新の記録を書けなかった"),
                self.keys.topic_key(),
                self.keys.body_key(),
            )
            .expect("レコードを封じられなかった")
            .into_bytes()
        }
    }

    fn edit(document: &mut Document, text: &str) -> Update {
        let end = document.text().len();
        document
            .apply_local(end..end, text)
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わったのに更新が無い")
    }

    /// Loroの文書を直接使い、書き手のIDを指定した更新を作る。
    fn raw_update(peers: &[u64]) -> Update {
        let doc = LoroDoc::new();
        let before = doc.oplog_vv();
        for peer in peers {
            doc.set_peer_id(*peer)
                .expect("書き手のIDを設定できなかった");
            doc.get_text("body")
                .insert(0, "x")
                .expect("本文に書けなかった");
            doc.commit();
        }
        Update::from_bytes(
            doc.export(ExportMode::updates(&before))
                .expect("更新を書き出せなかった"),
        )
    }

    #[test]
    fn 規則をすべて満たした更新を当てられる() {
        let fixture = Fixture::new();
        let (writer, mut local) = fixture.open(A);
        let update = edit(&mut local, "本文");
        let bytes = fixture.seal(&writer, &update, fixture.keys.body_key());
        let (_, mut remote) = fixture.open(D);

        let checked = fixture
            .check(bytes.clone())
            .expect("規則を満たしたレコードを拒んだ");
        assert_eq!(
            checked.author(),
            fixture.users[A].0,
            "著者が書いたメンバーと異なる"
        );
        assert_eq!(
            checked.writer(),
            writer.id(),
            "書き手のIDが書いた端末と異なる"
        );
        fixture
            .receive(&mut remote, bytes)
            .expect("規則を満たしたレコードを当てられなかった");
        assert_eq!(remote.text(), "本文", "当てた本文が書いた本文と異なる");
    }

    #[test]
    fn 著者を外したあとでも認証済みの公開鍵で同じログを確かめられる() {
        let fixture = Fixture::new();
        let (writer, mut local) = fixture.open(A);
        let author_id = fixture.users[A].0;
        let bytes = fixture.seal(&writer, &edit(&mut local, "本文"), fixture.keys.body_key());
        let before = fixture
            .check(bytes.clone())
            .expect("在籍中の著者が書いた更新を拒んだ");

        let removed = fixture
            .membership
            .get(&author_id)
            .expect("外す著者がメンバー構成にいなかった");
        let current = Membership::new(
            fixture
                .membership
                .iter()
                .filter(|member| member.id != author_id)
                .cloned()
                .collect(),
        )
        .expect("著者を外したあとのメンバー構成を作れなかった");

        let after = check_log_record(bytes, &fixture.view(), |topic, author| {
            if topic != fixture.topic {
                return None;
            }
            current
                .get(&author)
                .or_else(|| (author == removed.id).then_some(removed))
                .map(|member| member.keys.signing)
        })
        .expect("外した著者の認証済みの公開鍵で更新を確かめられなかった");
        assert_eq!(
            before, after,
            "著者をメンバー構成から外すと検査済みの更新が変わった",
        );
    }

    #[test]
    fn 書き手を設定していないか別の書き手が書いた更新を封じない() {
        let fixture = Fixture::new();
        let (writer, _) = fixture.open(A);
        let (_, mut other) = fixture.open(B);
        let mut unset = Document::from_snapshot(&fixture.base).expect("文書を開けなかった");

        for (case, update) in [
            ("書き手を設定していない文書", edit(&mut unset, "本文")),
            ("別の書き手の文書", edit(&mut other, "本文")),
        ] {
            let result = writer.seal_update(
                fixture.header(Branch::Body),
                &update,
                fixture.keys.topic_key(),
                fixture.keys.body_key(),
            );
            assert!(
                matches!(result, Err(Error::WriterIdMismatch)),
                "{case}の更新で、期待したエラーにならなかった: {result:?}",
            );
        }
    }

    #[test]
    fn 同じ利用者の2台の端末が同時に書いても収束する() {
        let fixture = Fixture::new();
        let (first_writer, mut first) = fixture.open(A);
        let (second_writer, mut second) = fixture.open(A);
        assert_ne!(
            first_writer.id(),
            second_writer.id(),
            "同じ利用者の2台の端末で書き手のIDが重なった",
        );

        let from_first = fixture.seal(
            &first_writer,
            &edit(&mut first, "1台目"),
            fixture.keys.body_key(),
        );
        let from_second = fixture.seal(
            &second_writer,
            &edit(&mut second, "2台目"),
            fixture.keys.body_key(),
        );
        fixture
            .receive(&mut first, from_second.clone())
            .expect("2台目の更新を当てられなかった");
        fixture
            .receive(&mut second, from_first.clone())
            .expect("1台目の更新を当てられなかった");
        let (_, mut third) = fixture.open(B);
        for bytes in [from_first, from_second] {
            fixture
                .receive(&mut third, bytes)
                .expect("ほかの端末の更新を当てられなかった");
        }

        assert_eq!(
            first.text(),
            second.text(),
            "同じ利用者の2台の端末の文書が食い違った"
        );
        assert_eq!(
            first.text(),
            third.text(),
            "ほかのメンバーの文書が食い違った"
        );
        assert!(
            first.text().contains("1台目") && first.text().contains("2台目"),
            "片方の端末の書き込みが失われた: {}",
            first.text(),
        );
    }

    #[test]
    fn 書き手のidを利用者の鍵だけから導くと_同じ利用者の2台の同時の書き込みで食い違う() {
        // 書き手のIDを利用者の鍵だけから導いた場合を、2台の端末に同じIDを設定して表す。
        let fixture = Fixture::new();
        let shared = WriterId::derive(
            &fixture.users[A].1.public_keys().signing,
            &WriterNonce::from_bytes([0; 16]),
        );
        let mut first = Document::from_snapshot(&fixture.base).expect("文書を開けなかった");
        let mut second = Document::from_snapshot(&fixture.base).expect("文書を開けなかった");
        first
            .set_writer(shared)
            .expect("書き手のIDを設定できなかった");
        second
            .set_writer(shared)
            .expect("書き手のIDを設定できなかった");

        let from_first = edit(&mut first, "1台目");
        let from_second = edit(&mut second, "2台目");
        first
            .apply_remote(&from_second)
            .expect("2台目の更新を当てられなかった");
        second
            .apply_remote(&from_first)
            .expect("1台目の更新を当てられなかった");

        // 同じ書き手のIDと番号の操作を、Loroは同じ操作として扱い、あとから届いたほうを当てない。
        // 各端末は自分の書き込みだけを持ち続け、互いの書き込みは失われる。
        assert_eq!(
            first.text(),
            "1台目",
            "1台目の文書に2台目の書き込みが入った"
        );
        assert_eq!(
            second.text(),
            "2台目",
            "2台目の文書に1台目の書き込みが入った"
        );
    }

    #[test]
    fn 知らない封筒の版のレコードを当てない() {
        let fixture = Fixture::new();
        let (writer, mut local) = fixture.open(A);
        let mut bytes = fixture.seal(&writer, &edit(&mut local, "本文"), fixture.keys.body_key());
        bytes[4..6].copy_from_slice(&2u16.to_be_bytes());
        let (_, mut remote) = fixture.open(D);

        let result = fixture.receive(&mut remote, bytes);
        assert!(
            matches!(result, Err(Error::UnsupportedEnvelopeVersion(2))),
            "知らない封筒の版のレコードで、期待したエラーにならなかった: {result:?}",
        );
        assert_eq!(
            remote.text(),
            "",
            "知らない封筒の版のレコードで文書が変わった"
        );
    }

    #[test]
    fn 提案の書き込みの鍵で署名した本文への書き込みを当てない() {
        let fixture = Fixture::new();
        let (writer, mut local) = fixture.open(C);
        let bytes = fixture.seal(
            &writer,
            &edit(&mut local, "本文"),
            fixture.keys.proposal_key(),
        );
        let (_, mut remote) = fixture.open(D);

        let result = fixture.receive(&mut remote, bytes);
        assert!(
            matches!(result, Err(Error::InvalidWriteSignature)),
            "提案の書き込みの鍵で署名した本文への書き込みで、期待したエラーにならなかった: {result:?}",
        );
        assert_eq!(
            remote.text(),
            "",
            "提案の書き込みの鍵の本文への書き込みで文書が変わった"
        );
    }

    #[test]
    fn 提案の書き込みの鍵で署名した枝への書き込みは確かめられる() {
        let fixture = Fixture::new();
        let (writer, mut local) = fixture.open(C);
        let update = edit(&mut local, "提案");
        let branch = Branch::Proposal(BranchId::generate().expect("枝のIDを作れなかった"));
        let bytes = writer
            .seal_update(
                fixture.header(branch),
                &update,
                fixture.keys.topic_key(),
                fixture.keys.proposal_key(),
            )
            .expect("レコードを封じられなかった")
            .into_bytes();

        let checked = fixture
            .check(bytes)
            .expect("提案の書き込みの鍵で署名した枝への書き込みを拒んだ");
        assert_eq!(
            checked.header().branch,
            branch,
            "確かめた枝が書いた枝と異なる"
        );
    }

    #[test]
    fn 別のトピックの鍵で暗号化したレコードを当てない() {
        let fixture = Fixture::new();
        let (writer, mut local) = fixture.open(A);
        let other = GenerationKeys::generate().expect("世代の鍵を作れなかった");
        let bytes = writer
            .seal_update(
                fixture.header(Branch::Body),
                &edit(&mut local, "本文"),
                other.topic_key(),
                fixture.keys.body_key(),
            )
            .expect("レコードを封じられなかった")
            .into_bytes();
        let (_, mut remote) = fixture.open(D);

        let result = fixture.receive(&mut remote, bytes);
        assert!(
            matches!(result, Err(Error::DecryptRecord)),
            "別のトピックの鍵のレコードで、期待したエラーにならなかった: {result:?}",
        );
        assert_eq!(
            remote.text(),
            "",
            "別のトピックの鍵のレコードで文書が変わった"
        );
    }

    #[test]
    fn 渡した世代と違う世代のレコードを当てない() {
        let fixture = Fixture::new();
        let (writer, mut local) = fixture.open(A);
        let mut header = fixture.header(Branch::Body);
        header.generation = 2;
        let bytes = writer
            .seal_update(
                header,
                &edit(&mut local, "本文"),
                fixture.keys.topic_key(),
                fixture.keys.body_key(),
            )
            .expect("レコードを封じられなかった")
            .into_bytes();

        assert!(
            matches!(fixture.check(bytes), Err(Error::UnknownGeneration(2))),
            "渡した世代と違う世代のレコードを受け付けた",
        );
    }

    #[test]
    fn 著者の署名が著者の公開鍵と合わない更新を当てない() {
        let fixture = Fixture::new();
        let (_, mut local) = fixture.open(A);
        let update = edit(&mut local, "本文");
        // Bの鍵で署名し、著者のIDだけをAにした更新の記録。
        let (author, _) = &fixture.users[A];
        let (_, other) = &fixture.users[B];
        let entry = AuthoredUpdate::sign(
            fixture.topic,
            fixture.page,
            *author,
            other.signing(),
            WriterNonce::generate().expect("乱数を作れなかった"),
            update.as_bytes(),
        );
        let bytes = SealedRecord::seal(
            fixture.header(Branch::Body),
            &entry.encode().expect("更新の記録を書けなかった"),
            fixture.keys.topic_key(),
            fixture.keys.body_key(),
        )
        .expect("レコードを封じられなかった")
        .into_bytes();
        let (_, mut remote) = fixture.open(D);

        let result = fixture.receive(&mut remote, bytes);
        assert!(
            matches!(result, Err(Error::InvalidAuthorSignature)),
            "著者の署名が合わない更新で、期待したエラーにならなかった: {result:?}",
        );
        assert_eq!(
            remote.text(),
            "",
            "著者の署名が合わない更新で文書が変わった"
        );
    }

    #[test]
    fn 別のページに付け替えた著者の署名を当てない() {
        let fixture = Fixture::new();
        let (_, mut local) = fixture.open(A);
        let update = edit(&mut local, "本文");
        let (member, keys) = &fixture.users[A];
        // 別のページのために署名した更新の記録を、このページのレコードとして封じ直す。
        let entry = AuthoredUpdate::sign(
            fixture.topic,
            SyncPageId::generate().expect("ページのIDを作れなかった"),
            *member,
            keys.signing(),
            WriterNonce::generate().expect("乱数を作れなかった"),
            update.as_bytes(),
        );
        let bytes = SealedRecord::seal(
            fixture.header(Branch::Body),
            &entry.encode().expect("更新の記録を書けなかった"),
            fixture.keys.topic_key(),
            fixture.keys.body_key(),
        )
        .expect("レコードを封じられなかった")
        .into_bytes();

        assert!(
            matches!(fixture.check(bytes), Err(Error::InvalidAuthorSignature)),
            "別のページの著者の署名を受け付けた",
        );
    }

    #[test]
    fn 認証済みの公開鍵が無い著者の更新を当てない() {
        let fixture = Fixture::new();
        let outsider = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        // 受け手のメンバー構成にはいない、書き手の手元だけのメンバー。
        let member = Member {
            id: MemberId::generate().expect("メンバーのIDを作れなかった"),
            role: Role::Editor,
            keys: outsider.public_keys().clone(),
        };
        let writer = Writer::start(&member, &outsider).expect("書き始められなかった");
        let mut local = Document::from_snapshot(&fixture.base).expect("文書を開けなかった");
        local
            .set_writer(writer.id())
            .expect("書き手のIDを設定できなかった");
        let bytes = fixture.seal(&writer, &edit(&mut local, "本文"), fixture.keys.body_key());
        let (_, mut remote) = fixture.open(D);

        let result = fixture.receive(&mut remote, bytes);
        assert!(
            matches!(result, Err(Error::UnknownMember)),
            "認証済みの公開鍵が無い著者の更新で、期待したエラーにならなかった: {result:?}",
        );
        assert_eq!(
            remote.text(),
            "",
            "認証済みの公開鍵が無い著者の更新で文書が変わった"
        );
    }

    #[test]
    fn 書き手のidが著者の公開鍵と乱数から導いた値と合わない更新を当てない() {
        let fixture = Fixture::new();
        // Aの署名と乱数を付けるが、更新はBの書き手のIDで書いたもの。
        let (_, mut b_document) = fixture.open(B);
        let update = edit(&mut b_document, "本文");
        let bytes = fixture.seal_entry(
            A,
            WriterNonce::generate().expect("乱数を作れなかった"),
            &update,
        );
        let (_, mut remote) = fixture.open(D);

        let result = fixture.receive(&mut remote, bytes);
        assert!(
            matches!(result, Err(Error::WriterIdMismatch)),
            "書き手のIDが合わない更新で、期待したエラーにならなかった: {result:?}",
        );
        assert_eq!(
            remote.text(),
            "",
            "書き手のIDが合わない更新で文書が変わった"
        );
    }

    #[test]
    fn ほかの書き手の操作を含む更新を当てない() {
        let fixture = Fixture::new();
        let nonce = WriterNonce::generate().expect("乱数を作れなかった");
        let own = WriterId::derive(&fixture.users[A].1.public_keys().signing, &nonce);
        let update = raw_update(&[own.value(), own.value().wrapping_add(1)]);
        let bytes = fixture.seal_entry(A, nonce, &update);
        let (_, mut remote) = fixture.open(D);

        let result = fixture.receive(&mut remote, bytes);
        assert!(
            matches!(result, Err(Error::ForeignOperations)),
            "ほかの書き手の操作を含む更新で、期待したエラーにならなかった: {result:?}",
        );
        assert_eq!(
            remote.text(),
            "",
            "ほかの書き手の操作を含む更新で文書が変わった"
        );
    }

    #[test]
    fn スナップショットの封筒を更新として受け付けない() {
        let fixture = Fixture::new();
        let mut header = fixture.header(Branch::Body);
        header.kind = RecordKind::Snapshot { last_sequence: 1 };
        let bytes = SealedRecord::seal(
            header,
            b"snapshot",
            fixture.keys.topic_key(),
            fixture.keys.body_key(),
        )
        .expect("レコードを封じられなかった")
        .into_bytes();
        let (writer, mut local) = fixture.open(A);

        assert!(
            matches!(fixture.check(bytes), Err(Error::UnexpectedRecordKind)),
            "スナップショットの封筒を更新として受け付けた",
        );
        assert!(
            matches!(
                writer.seal_update(
                    header,
                    &edit(&mut local, "本文"),
                    fixture.keys.topic_key(),
                    fixture.keys.body_key()
                ),
                Err(Error::UnexpectedRecordKind)
            ),
            "スナップショットの封筒で更新を封じられた",
        );
    }

    #[test]
    fn タイトルの更新も規則で確かめて当てられる() {
        let fixture = Fixture::new();
        let (writer, mut local) = fixture.open(A);
        let update = local
            .set_title("はじめに")
            .expect("タイトルを変えられなかった")
            .expect("タイトルが変わったのに更新が無い");
        let bytes = fixture.seal(&writer, &update, fixture.keys.body_key());
        let (_, mut remote) = fixture.open(D);

        fixture
            .receive(&mut remote, bytes)
            .expect("タイトルの更新を当てられなかった");
        assert_eq!(
            remote.title(),
            "はじめに",
            "当てたタイトルが書いたタイトルと異なる"
        );
        assert_eq!(remote.text(), "", "タイトルの更新で本文が変わった");
    }

    #[test]
    fn 確かめた更新のデバッグ表示には封筒と更新の長さだけを出す() {
        let fixture = Fixture::new();
        let (writer, mut local) = fixture.open(A);
        let update = local
            .set_title("テスト用のタイトル")
            .expect("タイトルを変えられなかった")
            .expect("タイトルの更新を取り出せなかった");
        let bytes = fixture.seal(&writer, &update, fixture.keys.body_key());
        let checked = fixture.check(bytes).expect("タイトルの更新を拒んだ");

        assert_eq!(
            format!("{checked:?}"),
            format!(
                "CheckedUpdate {{ header: {:?}, update: Update {{ len: {}, .. }}, .. }}",
                checked.header(),
                update.as_bytes().len(),
            ),
            "確かめた更新のデバッグ表示に、暗号化した中身が出た",
        );
    }
}
