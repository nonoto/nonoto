//! 版の記録。編集の役割のメンバーが、ある時点の文書に署名した記録である (ADR #0009)。
//!
//! 版の記録は本文のログに置き、サーバーからは本文の更新と見分けられない。
//! ログのレコードの中身 (版の記録) は、次のバイト列にする。
//!
//! ```text
//! 中身の種類 (1) | 作ったメンバーのID (16) | 版の値の長さ (4) | 版の値 | 中身のハッシュ (32) | 作ったメンバーの署名 (64)
//! ```
//!
//! 署名の対象は、区切りの文字列・トピックのID・ページのID・作ったメンバーのID・版の値の長さ (8)・版の値・中身のハッシュである。
//! 中身のハッシュを入れるのは、版の値が書き手のIDと番号だけを指すためである。
//! 端末ごとに違う更新を同じIDで渡されて中身が食い違っても、ハッシュと照らせば気づける。

use std::fmt;

use sha2::{Digest, Sha256};
use zeroize::{ZeroizeOnDrop, Zeroizing};

use crate::crypto::{SIGNATURE_LEN, UserSigningKey, UserVerifyingKey};
use crate::document::{Version, VersionContent};
use crate::error::Error;
use crate::member::MemberId;
use crate::record::{ID_LEN, Reader, SyncPageId, TopicId};

/// ログのレコードの中身の種類のうち、版の記録の値。
pub(crate) const ENTRY_VERSION: u8 = 2;

/// 中身のハッシュの、用途の区切りの文字列。
const CONTENT_HASH_DOMAIN: &[u8] = b"nonoto/version/content-hash\0";

/// 版の記録の署名の対象の、用途の区切りの文字列。
const VERSION_SIGNATURE_DOMAIN: &[u8] = b"nonoto/version/signature\0";

/// 中身のハッシュの長さ (バイト)。SHA-256の長さ。
const HASH_LEN: usize = 32;

/// 版の時点の中身 (タイトルと本文) のハッシュ。
#[derive(Clone, PartialEq, Eq, ZeroizeOnDrop)]
pub struct ContentHash([u8; HASH_LEN]);

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContentHash").finish_non_exhaustive()
    }
}

impl ContentHash {
    /// タイトルと本文から、SHA-256で中身のハッシュを計る。
    ///
    /// タイトルと本文の境目がずれた中身が同じハッシュにならないよう、それぞれの前に長さを置く。
    pub fn of(content: &VersionContent) -> Self {
        Self::of_text(&content.title, &content.body)
    }

    pub(crate) fn of_text(title: &str, body: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(CONTENT_HASH_DOMAIN);
        for part in [title, body] {
            let len = u64::try_from(part.len())
                .expect("文字列の長さは、usizeが表せる範囲にあり、u64にも収まる");
            hasher.update(len.to_be_bytes());
            hasher.update(part.as_bytes());
        }
        Self(hasher.finalize().into())
    }

    /// 版の中身との照合や署名に使う、SHA-256のバイト列を借りる。
    pub fn as_bytes(&self) -> &[u8; HASH_LEN] {
        &self.0
    }
}

/// 版の記録の中身。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct VersionEntry {
    pub(crate) creator: MemberId,
    pub(crate) version: Version,
    pub(crate) content_hash: ContentHash,
    signature: Zeroizing<[u8; SIGNATURE_LEN]>,
}

impl fmt::Debug for VersionEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VersionEntry").finish_non_exhaustive()
    }
}

impl VersionEntry {
    pub(crate) fn sign(
        topic: &TopicId,
        page: &SyncPageId,
        creator: MemberId,
        signing: &UserSigningKey,
        version: Version,
        content_hash: ContentHash,
    ) -> Self {
        let message = signed_message(topic, page, &creator, &version, &content_hash);
        Self {
            creator,
            signature: Zeroizing::new(signing.sign(&message)),
            version,
            content_hash,
        }
    }

    /// 作ったメンバーの署名を、そのメンバーの利用者の公開鍵で確かめる。
    pub(crate) fn verify(
        &self,
        topic: &TopicId,
        page: &SyncPageId,
        creator_key: &UserVerifyingKey,
    ) -> Result<(), Error> {
        let message = signed_message(
            topic,
            page,
            &self.creator,
            &self.version,
            &self.content_hash,
        );
        creator_key
            .verify_author(&message, &self.signature)
            .map_err(|_| Error::InvalidVersionSignature)
    }

    pub(crate) fn encode(&self) -> Result<Zeroizing<Vec<u8>>, Error> {
        let version = self.version.as_bytes();
        let len = u32::try_from(version.len()).map_err(|_| Error::RecordTooLarge)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(
            1 + ID_LEN + 4 + version.len() + HASH_LEN + SIGNATURE_LEN,
        ));
        bytes.push(ENTRY_VERSION);
        bytes.extend_from_slice(self.creator.as_bytes());
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(version);
        bytes.extend_from_slice(self.content_hash.as_bytes());
        bytes.extend_from_slice(&self.signature[..]);
        Ok(bytes)
    }

    /// ログのレコードの中身から読む。版の記録でないか、形が合わなければ [`Error::MalformedRecord`] を返す。
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader::new(bytes);
        if reader.u8()? != ENTRY_VERSION {
            return Err(Error::MalformedRecord);
        }
        let creator = MemberId::from_bytes(reader.array()?);
        let len = usize::try_from(reader.u32()?).map_err(|_| Error::MalformedRecord)?;
        let version = Version::from_bytes(reader.take(len)?.to_vec());
        let content_hash = ContentHash(reader.array()?);
        let signature = Zeroizing::new(reader.array()?);
        if !reader.remaining().is_empty() {
            return Err(Error::MalformedRecord);
        }
        Ok(Self {
            creator,
            version,
            content_hash,
            signature,
        })
    }
}

fn signed_message(
    topic: &TopicId,
    page: &SyncPageId,
    creator: &MemberId,
    version: &Version,
    content_hash: &ContentHash,
) -> Zeroizing<Vec<u8>> {
    let version = version.as_bytes();
    let len = u64::try_from(version.len())
        .expect("版の値の長さは、usizeが表せる範囲にあり、u64にも収まる")
        .to_be_bytes();
    Zeroizing::new(
        [
            VERSION_SIGNATURE_DOMAIN,
            topic.as_bytes(),
            page.as_bytes(),
            creator.as_bytes(),
            &len,
            version,
            content_hash.as_bytes(),
        ]
        .concat(),
    )
}

#[cfg(test)]
mod tests {
    use loro::{Frontiers, ID};

    use super::*;
    use crate::author::Writer;
    use crate::crypto::{Suite, WriteSigningKey};
    use crate::document::{Document, Update};
    use crate::generation::GenerationKeys;
    use crate::member::{Member, Membership, Role, UserKeys};
    use crate::receive::{
        CheckedEntry, CheckedUpdate, CheckedVersion, GenerationView, check_log_entry,
        check_log_record,
    };
    use crate::record::{Branch, BranchId, RecordHeader, RecordKind, SealedRecord};

    /// 「テストのメンバー構成」のうち、A (編集)・C (提案)・D (閲覧)。世代は1と2を持つ。
    struct Fixture {
        users: Vec<(MemberId, UserKeys)>,
        membership: Membership,
        generations: [GenerationKeys; 2],
        topic: TopicId,
        page: SyncPageId,
    }

    const A: usize = 0;
    const C: usize = 1;
    const D: usize = 2;

    impl Fixture {
        fn new() -> Self {
            let roles = [Role::Editor, Role::Proposer, Role::Viewer];
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
            let generation = || GenerationKeys::generate().expect("世代の鍵を作れなかった");
            Self {
                users,
                membership,
                generations: [generation(), generation()],
                topic: TopicId::generate().expect("トピックのIDを作れなかった"),
                page: SyncPageId::generate().expect("ページのIDを作れなかった"),
            }
        }

        fn keys(&self, generation: u64) -> &GenerationKeys {
            &self.generations[usize::try_from(generation - 1).expect("世代の番号が大きすぎる")]
        }

        fn header(&self, branch: Branch, generation: u64) -> RecordHeader {
            RecordHeader {
                kind: RecordKind::Log,
                suite: Suite::CURRENT,
                topic: self.topic,
                page: self.page,
                branch,
                generation,
            }
        }

        fn writer(&self, index: usize) -> Writer {
            let (id, keys) = &self.users[index];
            let member = self.membership.get(id).expect("メンバー構成にいない");
            Writer::start(member, keys).expect("書き始められなかった")
        }

        fn view(&self, generation: u64) -> GenerationView<'_> {
            let keys = self.keys(generation);
            GenerationView {
                generation,
                write_keys: keys.write_keys(),
                topic_key: keys.topic_key(),
            }
        }

        fn author_key(&self, topic: TopicId, author: MemberId) -> Option<UserVerifyingKey> {
            if topic != self.topic {
                return None;
            }
            self.membership
                .get(&author)
                .map(|member| member.keys.signing)
        }

        fn check(&self, bytes: Vec<u8>, generation: u64) -> Result<CheckedEntry, Error> {
            check_log_entry(bytes, &self.view(generation), |topic, author| {
                self.author_key(topic, author)
            })
        }

        fn check_update(&self, bytes: Vec<u8>, generation: u64) -> CheckedUpdate {
            match self.check(bytes, generation) {
                Ok(CheckedEntry::Update(update)) => update,
                other => panic!("更新の記録として確かめられなかった: {other:?}"),
            }
        }

        fn check_version(&self, bytes: Vec<u8>, generation: u64) -> CheckedVersion {
            match self.check(bytes, generation) {
                Ok(CheckedEntry::Version(version)) => version,
                other => panic!("版の記録として確かめられなかった: {other:?}"),
            }
        }

        fn seal_update(
            &self,
            writer: &Writer,
            header: RecordHeader,
            update: &Update,
            write_key: &WriteSigningKey,
        ) -> Vec<u8> {
            writer
                .seal_update(
                    header,
                    update,
                    self.keys(header.generation).topic_key(),
                    write_key,
                )
                .expect("更新を封じられなかった")
                .into_bytes()
        }

        fn seal_version(&self, writer: &Writer, document: &Document, generation: u64) -> Vec<u8> {
            let keys = self.keys(generation);
            writer
                .seal_version(
                    self.header(Branch::Body, generation),
                    document,
                    keys.topic_key(),
                    keys.body_key(),
                )
                .expect("版の記録を封じられなかった")
                .into_bytes()
        }

        /// 作ったメンバーと中身のハッシュを指定した版の記録を、本文の書き込みの鍵で封じる。
        fn seal_version_entry(
            &self,
            creator: usize,
            signer: usize,
            version: Version,
            content_hash: ContentHash,
        ) -> Vec<u8> {
            let entry = VersionEntry::sign(
                &self.topic,
                &self.page,
                self.users[creator].0,
                self.users[signer].1.signing(),
                version,
                content_hash,
            );
            seal_entry(&entry, self.header(Branch::Body, 1), self.keys(1))
        }
    }

    fn seal_entry(entry: &VersionEntry, header: RecordHeader, keys: &GenerationKeys) -> Vec<u8> {
        SealedRecord::seal(
            header,
            &entry.encode().expect("版の記録を書けなかった"),
            keys.topic_key(),
            keys.body_key(),
        )
        .expect("レコードを封じられなかった")
        .into_bytes()
    }

    fn edit(document: &mut Document, text: &str) -> Update {
        let end = document.text().len();
        document
            .apply_local(end..end, text)
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わったのに更新が無い")
    }

    fn apply(document: &mut Document, update: &CheckedUpdate) {
        document
            .apply_remote(update.update())
            .expect("確かめた更新を当てられなかった");
    }

    /// 空の文書を、書き手のIDを設定して開く。空の文書は操作を持たないため、どの端末でも同じ土台になる。
    fn empty(writer: &Writer) -> Document {
        let mut document = Document::new("").expect("文書を作れなかった");
        document
            .set_writer(writer.id())
            .expect("書き手のIDを設定できなかった");
        document
    }

    fn content(title: &str, body: &str) -> VersionContent {
        VersionContent {
            title: title.to_owned(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn タイトルと本文の境目がずれた中身は別のハッシュになる() {
        assert_ne!(
            ContentHash::of(&content("ab", "c")),
            ContentHash::of(&content("a", "bc")),
            "タイトルと本文の境目がずれても同じハッシュになった",
        );
        assert_eq!(
            ContentHash::of(&content("題", "本文")),
            ContentHash::of(&content("題", "本文")),
            "同じ中身から別のハッシュになった",
        );
    }

    #[test]
    fn 中身のハッシュが固定値と一致する() {
        // Pythonのhashlibで、UTF-8のバイト長を入れて計算した値。
        let expected = b"\
            \xc9\x6c\xe9\x79\x17\x49\x8a\x2a\xfa\x7c\x08\x34\xe2\x08\xbc\xa9\
            \x13\xb1\x2a\x3c\x37\x9a\xd1\x50\xc6\x50\x1f\xe5\x32\xc2\x28\xe4";
        assert_eq!(
            ContentHash::of(&content("題", "本文")).as_bytes(),
            expected,
            "固定のタイトルと本文から計ったハッシュが変わった",
        );
    }

    #[test]
    fn 版の記録は固定のバイト列と互換である() {
        // 空の文書の版を、0から31までのバイトを種にしたテスト用の鍵でOpenSSLから署名した。
        let public_key = b"\
            \x03\xa1\x07\xbf\xf3\xce\x10\xbe\x1d\x70\xdd\x18\xe7\x4b\xc0\x99\
            \x67\xe4\xd6\x30\x9b\xa5\x0d\x5f\x1d\xdc\x86\x64\x12\x55\x31\xb8";
        let bytes = [
            b"\x02".as_slice(),
            &[3; 16],
            b"\x00\x00\x00\x01\x00",
            b"\
                \x53\xb5\x3f\xa5\xb9\xdc\x36\x55\xb0\xb4\xe3\x62\xa9\xde\x1e\x5c\
                \x75\xa4\x58\xc6\x5b\x3a\x46\x4d\x83\x38\x32\x25\x69\xdb\x7e\x5d",
            b"\
                \x54\x9f\x6b\x9c\x57\x40\x65\xdc\x65\x1f\xc4\x62\x9a\xea\x93\x57\
                \xfc\xb2\xcc\xb4\x65\x43\x6d\xc3\x67\x70\xaf\xa8\x95\x24\xe0\xe4\
                \x78\xb3\x6e\x42\xd2\x0a\xe5\xba\x96\xb3\x28\x65\xba\x34\xf2\x59\
                \xa0\x0f\xa8\x38\xa2\xbf\xd9\x78\xf4\xd6\x74\x45\x58\xcb\x0a\x0b",
        ]
        .concat();
        let topic = TopicId::from_bytes([1; 16]);
        let page = SyncPageId::from_bytes([2; 16]);
        let key = UserVerifyingKey::from_bytes(public_key).expect("公開鍵を読めなかった");
        let entry = VersionEntry::decode(&bytes).expect("固定の版の記録を読めなかった");

        entry
            .verify(&topic, &page, &key)
            .expect("固定の版の記録の署名を確かめられなかった");
        let document = Document::new("").expect("文書を作れなかった");
        assert_eq!(
            entry.version,
            document.version(),
            "固定の版の値が空の文書の版の値と異なる",
        );
        assert_eq!(
            entry.content_hash,
            ContentHash::of(&content("", "")),
            "固定の版の中身のハッシュが異なる",
        );
        assert_eq!(
            entry.encode().expect("版の記録を書けなかった").as_slice(),
            bytes,
            "固定の版の記録を書き直すとバイト列が変わった",
        );
    }

    #[test]
    fn 版の記録を読み直せて_署名を確かめられる() {
        let keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let topic = TopicId::from_bytes([1; 16]);
        let page = SyncPageId::from_bytes([2; 16]);
        let entry = VersionEntry::sign(
            &topic,
            &page,
            MemberId::from_bytes([3; 16]),
            keys.signing(),
            Version::from_bytes(vec![4; 10]),
            ContentHash::of(&content("題", "本文")),
        );
        let decoded = VersionEntry::decode(&entry.encode().expect("版の記録を書けなかった"))
            .expect("版の記録を読めなかった");

        assert_eq!(decoded, entry, "読み直した版の記録が書いたものと異なる");
        decoded
            .verify(&topic, &page, &keys.public_keys().signing)
            .expect("版の記録の署名を確かめられなかった");
        assert!(
            matches!(
                decoded.verify(
                    &topic,
                    &SyncPageId::from_bytes([9; 16]),
                    &keys.public_keys().signing
                ),
                Err(Error::InvalidVersionSignature)
            ),
            "別のページの版の記録として署名を受け付けた",
        );
    }

    #[test]
    fn 版の記録を本文のログに置き_受け手が版の中身をハッシュと照らして取り出せる() {
        let fixture = Fixture::new();
        let writer = fixture.writer(A);
        let mut local = empty(&writer);
        let body = fixture.seal_update(
            &writer,
            fixture.header(Branch::Body, 1),
            &edit(&mut local, "本文"),
            fixture.keys(1).body_key(),
        );
        let title = fixture.seal_update(
            &writer,
            fixture.header(Branch::Body, 1),
            &local
                .set_title("題")
                .expect("タイトルを変えられなかった")
                .expect("タイトルが変わったのに更新が無い"),
            fixture.keys(1).body_key(),
        );
        let version = fixture.check_version(fixture.seal_version(&writer, &local, 1), 1);
        // 版を作ったあとの編集は、版の中身に入らない。
        let after = fixture.seal_update(
            &writer,
            fixture.header(Branch::Body, 1),
            &edit(&mut local, "と追記"),
            fixture.keys(1).body_key(),
        );

        let mut remote = empty(&fixture.writer(D));
        assert!(
            matches!(version.content(&remote), Err(Error::VersionNotFound)),
            "版までの操作を持たない文書から、版の中身を取り出せた",
        );
        for bytes in [body, title, after] {
            apply(&mut remote, &fixture.check_update(bytes, 1));
        }

        assert_eq!(
            version.creator(),
            fixture.users[A].0,
            "版を作ったメンバーが異なる"
        );
        let content = version
            .content(&remote)
            .expect("版の中身を取り出せなかった");
        assert_eq!(
            (content.title.as_str(), content.body.as_str()),
            ("題", "本文"),
            "取り出した版の中身が、版を作った時点の中身と異なる",
        );
        assert_eq!(
            remote.text(),
            "本文と追記",
            "版のあとの編集が文書に当たらなかった"
        );
    }

    #[test]
    fn 中身のハッシュと合わない版の記録から中身を取り出さない() {
        let fixture = Fixture::new();
        let writer = fixture.writer(A);
        let mut local = empty(&writer);
        let body = fixture.seal_update(
            &writer,
            fixture.header(Branch::Body, 1),
            &edit(&mut local, "本文"),
            fixture.keys(1).body_key(),
        );
        // 作ったメンバーは、本文とは別の中身のハッシュに署名した。
        let wrong = ContentHash::of(&VersionContent {
            title: String::new(),
            body: "別の本文".to_owned(),
        });
        let version =
            fixture.check_version(fixture.seal_version_entry(A, A, local.version(), wrong), 1);
        let mut remote = empty(&fixture.writer(D));
        apply(&mut remote, &fixture.check_update(body, 1));

        assert!(
            matches!(version.content(&remote), Err(Error::VersionContentMismatch)),
            "中身のハッシュと合わない版の中身を取り出せた",
        );
        assert!(
            matches!(version.fork(&remote), Err(Error::VersionContentMismatch)),
            "中身のハッシュと合わない版から枝を開けた",
        );
    }

    #[test]
    fn 作ったメンバーの鍵と署名が合わない版の記録を拒む() {
        let fixture = Fixture::new();
        let local = empty(&fixture.writer(A));
        let content = local
            .content_at(&local.version())
            .expect("版の中身を取り出せなかった");
        // 作ったメンバーをAとし、Cの鍵で署名した版の記録。
        let bytes = fixture.seal_version_entry(A, C, local.version(), ContentHash::of(&content));

        assert!(
            matches!(fixture.check(bytes, 1), Err(Error::InvalidVersionSignature)),
            "作ったメンバーの鍵と合わない署名の版の記録を受け付けた",
        );
    }

    #[test]
    fn 編集提案の枝のログに置いた版の記録を拒む() {
        let fixture = Fixture::new();
        let writer = fixture.writer(A);
        let local = empty(&writer);
        let branch = Branch::Proposal(BranchId::generate().expect("枝のIDを作れなかった"));
        assert!(
            matches!(
                writer.seal_version(
                    fixture.header(branch, 1),
                    &local,
                    fixture.keys(1).topic_key(),
                    fixture.keys(1).proposal_key(),
                ),
                Err(Error::UnexpectedRecordKind)
            ),
            "編集提案の枝のログに版の記録を封じられた",
        );

        let content = local
            .content_at(&local.version())
            .expect("版の中身を取り出せなかった");
        let entry = VersionEntry::sign(
            &fixture.topic,
            &fixture.page,
            fixture.users[A].0,
            fixture.users[A].1.signing(),
            local.version(),
            ContentHash::of(&content),
        );
        let keys = fixture.keys(1);
        let bytes = SealedRecord::seal(
            fixture.header(branch, 1),
            &entry.encode().expect("版の記録を書けなかった"),
            keys.topic_key(),
            keys.proposal_key(),
        )
        .expect("レコードを封じられなかった")
        .into_bytes();
        assert!(
            matches!(fixture.check(bytes, 1), Err(Error::UnexpectedEntryKind)),
            "編集提案の枝のログの版の記録を受け付けた",
        );
    }

    #[test]
    fn 承認で枝の更新を著者の署名つきのまま本文に取り込み_承認までの本文の変更と合わさる() {
        let fixture = Fixture::new();
        let editor = fixture.writer(A);
        let mut body = empty(&editor);
        let mut body_log = vec![(
            fixture.seal_update(
                &editor,
                fixture.header(Branch::Body, 1),
                &edit(&mut body, "本文。"),
                fixture.keys(1).body_key(),
            ),
            1,
        )];
        let base_record = fixture.seal_version(&editor, &body, 1);
        let base = fixture.check_version(base_record.clone(), 1);
        body_log.push((base_record, 1));

        // Cが版から枝を作り、提案の書き込みの鍵で枝のログに書く (世代1)。
        let proposer = fixture.writer(C);
        let mut branch = base.fork(&body).expect("版から枝を作れなかった");
        branch
            .set_writer(proposer.id())
            .expect("書き手のIDを設定できなかった");
        let branch_id = Branch::Proposal(BranchId::generate().expect("枝のIDを作れなかった"));
        let proposal = fixture.seal_update(
            &proposer,
            fixture.header(branch_id, 1),
            &branch
                .apply_local(0..0, "提案。")
                .expect("枝に書けなかった")
                .expect("本文が変わったのに更新が無い"),
            fixture.keys(1).proposal_key(),
        );

        // 承認までに、Aが本文に書き足す。
        body_log.push((
            fixture.seal_update(
                &editor,
                fixture.header(Branch::Body, 1),
                &edit(&mut body, "追記。"),
                fixture.keys(1).body_key(),
            ),
            1,
        ));

        // 新しい世代 (2) になってから、Aが承認する。枝の更新を世代2で本文のログに封じ直し、取り込んだ時点を版にする。
        let keys = fixture.keys(2);
        let checked = fixture.check_update(proposal, 1);
        apply(&mut body, &checked);
        let header = fixture.header(Branch::Body, 2);
        let merged = checked
            .reseal(header, keys.topic_key(), keys.body_key())
            .expect("枝の更新を本文のログに封じ直せなかった");
        body_log.push((merged.into_bytes(), 2));
        let approved_record = editor
            .seal_approval_version(
                header,
                &body,
                &branch.version(),
                keys.topic_key(),
                keys.body_key(),
            )
            .expect("承認した時点の版の記録を封じられなかった");
        body_log.push((approved_record.into_bytes(), 2));

        // Dは本文のログだけを受け取り、Aと同じ文書になる。
        let mut remote = empty(&fixture.writer(D));
        let mut authors = Vec::new();
        let mut versions = Vec::new();
        for (bytes, generation) in body_log {
            match fixture
                .check(bytes, generation)
                .expect("本文のログのレコードを拒んだ")
            {
                CheckedEntry::Update(update) => {
                    authors.push(update.author());
                    apply(&mut remote, &update);
                }
                CheckedEntry::Version(version) => versions.push(version),
            }
        }

        assert_eq!(
            remote.text(),
            body.text(),
            "承認したあとの本文が端末ごとに食い違った"
        );
        assert!(
            remote.text().contains("提案。") && remote.text().contains("追記。"),
            "承認で、枝の更新か承認までの本文の変更が失われた: {}",
            remote.text(),
        );
        assert_eq!(
            authors.last(),
            Some(&fixture.users[C].0),
            "本文に取り込んだ枝の更新の著者が、提案したメンバーではない",
        );
        let approved = versions.last().expect("承認の版の記録が無い");
        assert_eq!(
            approved
                .content(&remote)
                .expect("承認の版の中身を取り出せなかった")
                .body
                .as_str(),
            body.text(),
            "承認の版の中身が、取り込んだ時点の本文と異なる",
        );
    }

    #[test]
    fn 枝の先端まで本文に当たるまで承認の版を作らない() {
        let fixture = Fixture::new();
        let editor = fixture.writer(A);
        let mut body = empty(&editor);
        edit(&mut body, "本文。");
        let base = fixture.check_version(fixture.seal_version(&editor, &body, 1), 1);
        let proposer = fixture.writer(C);
        let mut branch = base.fork(&body).expect("版から枝を作れなかった");
        branch
            .set_writer(proposer.id())
            .expect("書き手のIDを設定できなかった");
        let branch_id = Branch::Proposal(BranchId::generate().expect("枝のIDを作れなかった"));
        let keys = fixture.keys(1);
        let header = fixture.header(branch_id, 1);
        let first = fixture.seal_update(
            &proposer,
            header,
            &edit(&mut branch, "最初。"),
            keys.proposal_key(),
        );
        let second = fixture.seal_update(
            &proposer,
            header,
            &edit(&mut branch, "次。"),
            keys.proposal_key(),
        );
        let tip = branch.version();
        let seal_approval = |document: &Document| {
            editor.seal_approval_version(
                fixture.header(Branch::Body, 1),
                document,
                &tip,
                keys.topic_key(),
                keys.body_key(),
            )
        };

        apply(&mut body, &fixture.check_update(second, 1));
        assert_eq!(
            body.text(),
            "本文。",
            "先行する更新が無いのに本文が変わった"
        );
        assert!(
            matches!(seal_approval(&body), Err(Error::VersionNotFound)),
            "枝の更新が保留されているのに承認の版を作れた",
        );

        apply(&mut body, &fixture.check_update(first, 1));
        let approved = seal_approval(&body).expect("枝の先端まで当てたのに承認の版を作れなかった");
        let approved = fixture.check_version(approved.into_bytes(), 1);
        let content = approved
            .content(&body)
            .expect("承認の版の中身を取り出せなかった");
        assert_eq!(
            content.body, "本文。最初。次。",
            "承認の版に枝の更新がそろっていない",
        );
    }

    #[test]
    fn 提案の役割のメンバーは本文のログに書けない() {
        let fixture = Fixture::new();
        let proposer = fixture.writer(C);
        let mut local = empty(&proposer);
        let bytes = fixture.seal_update(
            &proposer,
            fixture.header(Branch::Body, 1),
            &edit(&mut local, "本文"),
            fixture.keys(1).proposal_key(),
        );

        assert!(
            matches!(fixture.check(bytes, 1), Err(Error::InvalidWriteSignature)),
            "提案の書き込みの鍵で署名した本文への書き込みを受け付けた",
        );
    }

    #[test]
    fn 捨てた枝は本文を変えず_枝のログからは枝の文書を読める() {
        let fixture = Fixture::new();
        let editor = fixture.writer(A);
        let mut body = empty(&editor);
        let original = fixture.seal_update(
            &editor,
            fixture.header(Branch::Body, 1),
            &edit(&mut body, "本文。"),
            fixture.keys(1).body_key(),
        );
        let base_record = fixture.seal_version(&editor, &body, 1);
        let base = fixture.check_version(base_record.clone(), 1);
        let following = fixture.seal_update(
            &editor,
            fixture.header(Branch::Body, 1),
            &edit(&mut body, "追記。"),
            fixture.keys(1).body_key(),
        );
        let proposer = fixture.writer(C);
        let mut branch = base.fork(&body).expect("版から枝を作れなかった");
        branch
            .set_writer(proposer.id())
            .expect("書き手のIDを設定できなかった");
        let branch_id = Branch::Proposal(BranchId::generate().expect("枝のIDを作れなかった"));
        let branch_log = [fixture.seal_update(
            &proposer,
            fixture.header(branch_id, 1),
            &edit(&mut branch, "提案。"),
            fixture.keys(1).proposal_key(),
        )];

        // Dは本文のログを当て、枝は本文から分岐した別の文書として読む。枝を捨てても本文には何も当てない。
        let mut remote = empty(&fixture.writer(D));
        for bytes in [original, following] {
            apply(&mut remote, &fixture.check_update(bytes, 1));
        }
        let remote_base = fixture.check_version(base_record, 1);
        let mut remote_branch = remote_base.fork(&remote).expect("版から枝を作れなかった");
        for bytes in branch_log {
            apply(&mut remote_branch, &fixture.check_update(bytes, 1));
        }

        assert_eq!(
            remote_branch.text(),
            "本文。提案。",
            "枝のログから枝の文書を読めなかった"
        );
        drop(remote_branch);
        assert_eq!(
            remote.text(),
            "本文。追記。",
            "捨てた枝の更新で本文が変わった"
        );
    }

    #[test]
    fn 別のページを指す封筒には封じ直さない() {
        let fixture = Fixture::new();
        let writer = fixture.writer(A);
        let mut local = empty(&writer);
        let checked = fixture.check_update(
            fixture.seal_update(
                &writer,
                fixture.header(Branch::Body, 1),
                &edit(&mut local, "本文"),
                fixture.keys(1).body_key(),
            ),
            1,
        );
        let mut other = fixture.header(Branch::Body, 2);
        other.page = SyncPageId::generate().expect("ページのIDを作れなかった");
        let keys = fixture.keys(2);

        assert!(
            matches!(
                checked.reseal(other, keys.topic_key(), keys.body_key()),
                Err(Error::ResealTargetMismatch)
            ),
            "別のページを指す封筒に封じ直せた",
        );
    }

    #[test]
    fn 別のトピックを指す封筒とログでない封筒には封じ直さない() {
        let fixture = Fixture::new();
        let writer = fixture.writer(A);
        let mut local = empty(&writer);
        let checked = fixture.check_update(
            fixture.seal_update(
                &writer,
                fixture.header(Branch::Body, 1),
                &edit(&mut local, "本文"),
                fixture.keys(1).body_key(),
            ),
            1,
        );
        let mut other_topic = fixture.header(Branch::Body, 1);
        other_topic.topic = TopicId::generate().expect("トピックのIDを作れなかった");
        let mut snapshot = fixture.header(Branch::Body, 1);
        snapshot.kind = RecordKind::Snapshot { last_sequence: 1 };
        let keys = fixture.keys(1);

        assert!(
            matches!(
                checked.reseal(other_topic, keys.topic_key(), keys.body_key()),
                Err(Error::ResealTargetMismatch)
            ),
            "別のトピックを指す封筒に封じ直せた",
        );
        assert!(
            matches!(
                checked.reseal(snapshot, keys.topic_key(), keys.body_key()),
                Err(Error::UnexpectedRecordKind)
            ),
            "スナップショットの封筒に封じ直せた",
        );
    }

    #[test]
    fn 知らない中身の種類のレコードを当てない() {
        let fixture = Fixture::new();
        let mut remote = empty(&fixture.writer(D));
        let keys = fixture.keys(1);

        for (case, payload) in [
            ("知らない種類の値の中身", b"\x03payload".as_slice()),
            ("空の中身", b"".as_slice()),
        ] {
            let bytes = SealedRecord::seal(
                fixture.header(Branch::Body, 1),
                payload,
                keys.topic_key(),
                keys.body_key(),
            )
            .expect("レコードを封じられなかった")
            .into_bytes();
            let result = fixture.check(bytes, 1).and_then(|entry| match entry {
                CheckedEntry::Update(update) => remote.apply_remote(update.update()).map(|_| ()),
                CheckedEntry::Version(_) => Ok(()),
            });

            assert!(
                matches!(result, Err(Error::MalformedRecord)),
                "{case}のレコードで、期待したエラーにならなかった: {result:?}",
            );
            assert_eq!(remote.text(), "", "{case}のレコードで文書が変わった");
        }
    }

    #[test]
    fn 形の正しくない版の値を拒む() {
        let document = Document::new("本文").expect("文書を作れなかった");
        assert!(
            matches!(
                document.content_at(&Version::from_bytes(vec![0xff; 3])),
                Err(Error::InvalidVersion)
            ),
            "形の正しくない版の値を受け付けた",
        );
    }

    #[test]
    fn 正規の形式でない版の記録を受信時に拒む() {
        let fixture = Fixture::new();
        let local = empty(&fixture.writer(A));
        let mut bytes = local.version().as_bytes().to_vec();
        bytes.push(0);
        let invalid_counter = Frontiers::from_id(ID::new(1, -1)).encode();

        for (case, bytes) in [
            ("余りのある版の値", bytes),
            ("操作番号が範囲外の版の値", invalid_counter),
        ] {
            let record = fixture.seal_version_entry(
                A,
                A,
                Version::from_bytes(bytes),
                ContentHash::of(&content("", "")),
            );
            assert!(
                matches!(fixture.check(record, 1), Err(Error::InvalidVersion)),
                "{case}を受信時に拒まなかった",
            );
        }
    }

    #[test]
    fn 更新だけを受け付ける入口で版の記録を拒む() {
        let fixture = Fixture::new();
        let writer = fixture.writer(A);
        let local = empty(&writer);
        let bytes = fixture.seal_version(&writer, &local, 1);
        let result = check_log_record(bytes, &fixture.view(1), |topic, author| {
            fixture.author_key(topic, author)
        });

        assert!(
            matches!(result, Err(Error::UnexpectedEntryKind)),
            "版の記録を更新として受け付けた: {result:?}",
        );
    }

    #[test]
    fn 版のデバッグ表示には復号した中身を出さない() {
        let fixture = Fixture::new();
        let writer = fixture.writer(A);
        let mut local = empty(&writer);
        edit(&mut local, "テスト用の本文");
        let checked = fixture.check_version(fixture.seal_version(&writer, &local, 1), 1);

        assert_eq!(
            format!("{checked:?}"),
            format!("CheckedVersion {{ header: {:?}, .. }}", checked.header()),
            "確認済みの版のデバッグ表示に封筒以外の値が出た",
        );
        assert_eq!(
            format!("{:?}", checked.version()),
            "Version { .. }",
            "版を指すバイト列がデバッグ表示に出た",
        );
        assert_eq!(
            format!("{:?}", checked.content_hash()),
            "ContentHash { .. }",
            "中身のハッシュがデバッグ表示に出た",
        );
    }
}
