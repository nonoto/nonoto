//! 書き手のIDと、著者の署名を付けた更新。
//!
//! 書き手のIDは、利用者の署名の公開鍵と、端末が書き始めるときに作る乱数から導く。
//! 同じ利用者の2台の端末が同時に書いても、書き手のIDが重ならないようにするためである。
//! 乱数は著者の署名の対象に入れて、更新と一緒に暗号化する。
//!
//! ログのレコードの中身 (更新の記録) は、次のバイト列にする。
//!
//! ```text
//! 中身の種類 (1) | 著者のメンバーのID (16) | 書き始めの乱数 (16) | 更新の長さ (4) | 更新 | 著者の署名 (64)
//! ```
//!
//! 著者の署名の対象は、区切りの文字列・トピックのID・ページのID・著者のメンバーのID・乱数・更新である。
//! 枝のIDと世代の番号は入れず、承認で枝の更新を本文に書き直すときと、新しい世代で暗号化し直すときにも署名をそのまま使えるようにする。

use std::fmt;

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::crypto::{SIGNATURE_LEN, TopicKey, UserSigningKey, UserVerifyingKey, WriteSigningKey};
use crate::document::{Document, Update, Version, check_update_writer};
use crate::error::Error;
use crate::member::{Member, MemberId, UserKeys};
use crate::record::{
    Branch, ID_LEN, Reader, RecordHeader, RecordKind, SealedRecord, SyncPageId, TopicId, random_id,
};
use crate::version::{ContentHash, VersionEntry};

/// 書き手のIDを導くときの、用途の区切りの文字列。
const WRITER_DOMAIN: &[u8] = b"nonoto/author/writer-id\0";

/// 著者の署名の対象の、用途の区切りの文字列。
const AUTHOR_SIGNATURE_DOMAIN: &[u8] = b"nonoto/author/update-signature\0";

/// ログのレコードの中身の種類のうち、更新の記録の値。
pub(crate) const ENTRY_UPDATE: u8 = 1;

random_id! {
    /// 端末が書き始めるときに作る乱数。書き手のIDを導くのに使う。
    WriterNonce
}

/// CRDT (Loro) の書き手のID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WriterId(u64);

impl WriterId {
    /// 利用者の署名の公開鍵と書き始めの乱数から、書き手のIDを導く。
    ///
    /// どの端末でも、同じ公開鍵と乱数から同じIDになる。
    pub fn derive(author: &UserVerifyingKey, nonce: &WriterNonce) -> Self {
        let digest = Sha256::new()
            .chain_update(WRITER_DOMAIN)
            .chain_update(author.to_bytes())
            .chain_update(nonce.as_bytes())
            .finalize();
        let value = u64::from_be_bytes(
            digest[..8]
                .try_into()
                .expect("SHA-256の値の先頭8バイトを切り出したため、長さは8と一致する"),
        );
        // Loroは u64::MAX を書き手のIDに使えないため、その値だけ1つ手前に寄せる。
        Self(value.min(u64::MAX - 1))
    }

    /// Loroに渡す値。
    pub fn value(self) -> u64 {
        self.0
    }
}

/// 端末が文書に書き始めてから持つ、書き手の情報。
///
/// 書き始めの単位は文書を開くごとで、開くたびに新しい乱数で作り直す。
#[derive(Clone)]
pub struct Writer {
    member: MemberId,
    signing: UserSigningKey,
    nonce: WriterNonce,
    id: WriterId,
}

impl fmt::Debug for Writer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Writer").finish_non_exhaustive()
    }
}

impl Writer {
    /// メンバー構成の項目のメンバーとして、新しい乱数で書き始める。
    ///
    /// 返した書き手のIDを [`crate::Document::set_writer`] で文書に設定してから書く。
    /// メンバーの署名の公開鍵が利用者の鍵と合わなければ [`Error::AuthorKeyMismatch`] を返す。
    /// 受け手は著者の署名をメンバーの公開鍵で確かめるため、合わない鍵で書いた更新はどの端末も当てない。
    pub fn start(member: &Member, keys: &UserKeys) -> Result<Self, Error> {
        if member.keys.signing != keys.public_keys().signing {
            return Err(Error::AuthorKeyMismatch);
        }
        let nonce = WriterNonce::generate()?;
        Ok(Self {
            member: member.id,
            signing: keys.signing().clone(),
            id: WriterId::derive(&keys.public_keys().signing, &nonce),
            nonce,
        })
    }

    /// 手元の文書に設定する書き手のIDを返す。
    pub fn id(&self) -> WriterId {
        self.id
    }

    /// 書いているメンバー。
    pub(crate) fn member(&self) -> MemberId {
        self.member
    }

    /// 書いているメンバーの、利用者の署名の鍵。
    pub(crate) fn signing_key(&self) -> &UserSigningKey {
        &self.signing
    }

    /// 手元の更新に著者の署名を付け、ログのレコードとして暗号化して書き込みの鍵で署名する。
    ///
    /// 封筒の種類がログのレコードでなければ [`Error::UnexpectedRecordKind`] を返す。
    /// 更新がこの書き手の操作だけを含むことを確かめてから署名する。
    pub fn seal_update(
        &self,
        header: RecordHeader,
        update: &Update,
        topic_key: &TopicKey,
        write_key: &WriteSigningKey,
    ) -> Result<SealedRecord, Error> {
        if header.kind != RecordKind::Log {
            return Err(Error::UnexpectedRecordKind);
        }
        check_update_writer(update.as_bytes(), self.id)?;
        let entry = AuthoredUpdate::sign(
            header.topic,
            header.page,
            self.member,
            &self.signing,
            self.nonce,
            update.as_bytes(),
        );
        SealedRecord::seal(header, &entry.encode()?, topic_key, write_key)
    }

    /// 文書の今の時点を版にし、版の値と中身のハッシュに利用者の鍵で署名した版の記録を、本文のログのレコードとして封じる。
    ///
    /// 版の記録は本文のログに置くため、封筒が本文のログのレコードでなければ [`Error::UnexpectedRecordKind`] を返す。
    /// 書き込みの鍵には本文の書き込みの鍵を渡す。
    /// 編集提案の承認では [`Writer::seal_approval_version`] を使う。
    pub fn seal_version(
        &self,
        header: RecordHeader,
        document: &Document,
        topic_key: &TopicKey,
        write_key: &WriteSigningKey,
    ) -> Result<SealedRecord, Error> {
        let version = document.version();
        let content_hash = ContentHash::of(&document.content_at(&version)?);
        self.seal_version_entry(header, version, content_hash, topic_key, write_key)
    }

    /// 文書から取り出しておいた版の値と中身のハッシュに署名し、版の記録として封じる。
    ///
    /// 同期の処理が、版を付けた時点と送る時点の間に文書が変わっても、付けた時点の版を送るのに使う。
    pub(crate) fn seal_version_entry(
        &self,
        header: RecordHeader,
        version: Version,
        content_hash: ContentHash,
        topic_key: &TopicKey,
        write_key: &WriteSigningKey,
    ) -> Result<SealedRecord, Error> {
        if header.kind != RecordKind::Log || header.branch != Branch::Body {
            return Err(Error::UnexpectedRecordKind);
        }
        let entry = VersionEntry::sign(
            &header.topic,
            &header.page,
            self.member,
            &self.signing,
            version,
            content_hash,
        );
        SealedRecord::seal(header, &entry.encode()?, topic_key, write_key)
    }

    /// 編集提案の枝の先端まで本文に当たったことを確かめ、承認した時点を版の記録として封じる。
    ///
    /// `proposal_tip` には承認する枝の先端の版の値を渡す。
    /// 依存する更新がまだ本文に当たっていなければ [`Error::VersionNotFound`] を返す。
    /// 封筒と鍵は [`Writer::seal_version`] と同じように渡す。
    pub fn seal_approval_version(
        &self,
        header: RecordHeader,
        document: &Document,
        proposal_tip: &Version,
        topic_key: &TopicKey,
        write_key: &WriteSigningKey,
    ) -> Result<SealedRecord, Error> {
        document.require_version(proposal_tip)?;
        self.seal_version(header, document, topic_key, write_key)
    }
}

/// 著者の署名を付けた更新。平文の更新は、元の更新か復号した中身から借りる。
#[derive(PartialEq, Eq)]
pub(crate) struct AuthoredUpdate<'a> {
    pub(crate) author: MemberId,
    pub(crate) nonce: WriterNonce,
    pub(crate) update: &'a [u8],
    signature: [u8; SIGNATURE_LEN],
}

impl<'a> AuthoredUpdate<'a> {
    pub(crate) fn sign(
        topic: TopicId,
        page: SyncPageId,
        author: MemberId,
        signing: &UserSigningKey,
        nonce: WriterNonce,
        update: &'a [u8],
    ) -> Self {
        let message = signed_message(&topic, &page, &author, &nonce, update);
        Self {
            author,
            nonce,
            signature: signing.sign(&message),
            update,
        }
    }

    /// 確かめた更新の記録を、著者の署名をそのまま使って組み直す。承認と暗号化し直しで使う。
    pub(crate) fn from_parts(
        author: MemberId,
        nonce: WriterNonce,
        update: &'a [u8],
        signature: [u8; SIGNATURE_LEN],
    ) -> Self {
        Self {
            author,
            nonce,
            update,
            signature,
        }
    }

    pub(crate) fn signature(&self) -> [u8; SIGNATURE_LEN] {
        self.signature
    }

    /// 著者の署名を、著者の利用者の公開鍵で確かめる。
    pub(crate) fn verify(
        &self,
        topic: &TopicId,
        page: &SyncPageId,
        author_key: &UserVerifyingKey,
    ) -> Result<(), Error> {
        let message = signed_message(topic, page, &self.author, &self.nonce, self.update);
        author_key.verify_author(&message, &self.signature)
    }

    pub(crate) fn encode(&self) -> Result<Zeroizing<Vec<u8>>, Error> {
        let update = self.update;
        let len = u32::try_from(update.len()).map_err(|_| Error::RecordTooLarge)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(
            1 + 2 * ID_LEN + 4 + update.len() + SIGNATURE_LEN,
        ));
        bytes.push(ENTRY_UPDATE);
        bytes.extend_from_slice(self.author.as_bytes());
        bytes.extend_from_slice(self.nonce.as_bytes());
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(update);
        bytes.extend_from_slice(&self.signature);
        Ok(bytes)
    }

    /// ログのレコードの中身から読む。更新の記録でないか、形が合わなければ [`Error::MalformedRecord`] を返す。
    pub(crate) fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut reader = Reader::new(bytes);
        if reader.u8()? != ENTRY_UPDATE {
            return Err(Error::MalformedRecord);
        }
        let author = MemberId::from_bytes(reader.array()?);
        let nonce = WriterNonce::from_bytes(reader.array()?);
        let len = usize::try_from(reader.u32()?).map_err(|_| Error::MalformedRecord)?;
        let update = reader.take(len)?;
        let signature = reader.array()?;
        if !reader.remaining().is_empty() {
            return Err(Error::MalformedRecord);
        }
        Ok(Self {
            author,
            nonce,
            update,
            signature,
        })
    }
}

fn signed_message(
    topic: &TopicId,
    page: &SyncPageId,
    author: &MemberId,
    nonce: &WriterNonce,
    update: &[u8],
) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(
        [
            AUTHOR_SIGNATURE_DOMAIN,
            topic.as_bytes(),
            page.as_bytes(),
            author.as_bytes(),
            nonce.as_bytes(),
            update,
        ]
        .concat(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Suite;
    use crate::member::Role;

    /// 0から31までのバイトを種にしたテスト用Ed25519鍵の公開鍵 (OpenSSLで生成)。
    const PUBLIC_KEY: &[u8; 32] = b"\
        \x03\xa1\x07\xbf\xf3\xce\x10\xbe\x1d\x70\xdd\x18\xe7\x4b\xc0\x99\
        \x67\xe4\xd6\x30\x9b\xa5\x0d\x5f\x1d\xdc\x86\x64\x12\x55\x31\xb8";

    #[test]
    fn 固定の公開鍵と乱数から既知の書き手のidを導く() {
        let key = UserVerifyingKey::from_bytes(PUBLIC_KEY).expect("利用者の公開鍵を読めなかった");
        let nonce = WriterNonce::from_bytes([4; 16]);

        assert_eq!(
            WriterId::derive(&key, &nonce).value(),
            0x99a2_b215_c678_3033,
            "固定の公開鍵と乱数から導いた書き手のIDが変わった",
        );
    }

    #[test]
    fn 更新の記録は固定のバイト列と互換である() {
        // トピックIDは1、ページIDは2、著者IDは3、乱数は4を16バイトずつ並べる。
        // 最後の64バイトはOpenSSLで付けた著者の署名。
        let bytes = [
            b"\x01".as_slice(),
            &[3; 16],
            &[4; 16],
            b"\x00\x00\x00\x04\x05\x06\x07\x08",
            b"\
                \x7b\x50\xda\xec\xaa\xeb\xf6\x31\xcd\x16\x9b\x65\xd6\xf7\x05\xc9\
                \x3d\x38\xfe\xbc\x73\x63\x3a\x58\x0e\x60\x91\xed\x0a\x6d\xa6\xfb\
                \x76\x6e\xbc\x1e\xf5\x62\xf6\xba\x05\x79\x4a\xab\xfb\x71\xa1\x9f\
                \x90\x49\x65\x67\x6e\x16\x6e\x2b\x72\x7f\x1e\x3f\x99\xbe\x64\x05",
        ]
        .concat();
        let topic = TopicId::from_bytes([1; 16]);
        let page = SyncPageId::from_bytes([2; 16]);
        let key = UserVerifyingKey::from_bytes(PUBLIC_KEY).expect("利用者の公開鍵を読めなかった");

        let entry = AuthoredUpdate::decode(&bytes).expect("固定の更新の記録を読めなかった");
        assert_eq!(
            entry.update,
            [5, 6, 7, 8],
            "固定の更新の記録から違う更新を読み出した",
        );
        entry
            .verify(&topic, &page, &key)
            .expect("固定の更新の記録の著者の署名が合わなかった");
        let encoded = entry.encode().expect("更新の記録を書けなかった");
        assert_eq!(
            encoded.as_slice(),
            bytes,
            "固定の更新の記録を書き直したところバイト列が変わった",
        );
    }

    #[test]
    fn メンバーの公開鍵と合わない利用者の鍵では書き始められない() {
        let registered = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let other = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let member = Member {
            id: MemberId::from_bytes([3; 16]),
            role: Role::Editor,
            keys: registered.public_keys().clone(),
        };

        assert!(
            matches!(
                Writer::start(&member, &other),
                Err(Error::AuthorKeyMismatch)
            ),
            "メンバーの公開鍵と合わない利用者の鍵で書き始められた",
        );
        Writer::start(&member, &registered)
            .expect("メンバーの公開鍵と合う利用者の鍵で書き始められなかった");
    }

    #[test]
    fn 同じ公開鍵と乱数から同じ書き手のidを導く() {
        let keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let nonce = WriterNonce::generate().expect("乱数を作れなかった");
        let other = WriterNonce::generate().expect("乱数を作れなかった");
        let key = &keys.public_keys().signing;

        assert_eq!(
            WriterId::derive(key, &nonce),
            WriterId::derive(key, &nonce),
            "同じ入力から別の書き手のIDが導かれた",
        );
        assert_ne!(
            WriterId::derive(key, &nonce),
            WriterId::derive(key, &other),
            "乱数が違っても同じ書き手のIDになった",
        );
    }

    #[test]
    fn 著者の署名を付けた更新を読み直せる() {
        let keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let entry = AuthoredUpdate::sign(
            TopicId::from_bytes([1; 16]),
            SyncPageId::from_bytes([2; 16]),
            MemberId::from_bytes([3; 16]),
            keys.signing(),
            WriterNonce::from_bytes([4; 16]),
            &[5; 10],
        );
        let bytes = entry.encode().expect("更新の記録を書けなかった");

        assert!(
            AuthoredUpdate::decode(&bytes).expect("更新の記録を読めなかった") == entry,
            "読み直した更新の記録が書いたものと異なる",
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(
            AuthoredUpdate::decode(&trailing).is_err(),
            "末尾に余りのある更新の記録を受け付けた",
        );
    }
}
