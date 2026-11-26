//! 同期で送るレコードの、封筒と中身の形式。
//!
//! レコードは、平文の封筒・nonce・暗号化した中身・書き込みの署名をこの順に並べたバイト列である。
//!
//! ```text
//! 封筒: 識別の印 (4) | 封筒の版 (2) | 種類 (1) | 方式のID (2)
//!       | トピックのID (16) | ページのID (16) | 枝 (1。編集提案なら続けて枝のID (16))
//!       | 世代の番号 (8) | (スナップショットだけ) まとめた順番の上限 (8)
//! 続き: nonce (24) | 暗号文 (中身 + 認証タグ (16)) | 書き込みの署名 (64)
//! 中身: 中身の版 (2) | 中身の長さ (4) | 中身 | 詰め物
//! ```
//!
//! 数はすべてビッグエンディアンで書く。
//! 封筒の先頭の識別の印と封筒の版は、どの版でも変えない (ADR #0010)。
//! 封筒は中身の暗号化の追加データと書き込みの署名の対象に入れ、方式のIDや封筒の値の書き換えを見つける。
//! 受け手は中身の長さのあとの詰め物を読み飛ばす。

use std::fmt;

use zeroize::{Zeroize, Zeroizing};

use crate::crypto::{
    NONCE_LEN, SIGNATURE_LEN, Suite, TAG_LEN, TopicKey, WriteSigningKey, WriteVerifyingKey,
};
use crate::error::Error;

/// 封筒の先頭に置く識別の印。
pub(crate) const MAGIC: [u8; 4] = *b"NNTR";

/// 今の封筒の版。
pub(crate) const ENVELOPE_VERSION: u16 = 1;

/// 今の中身の版。
pub(crate) const CONTENT_VERSION: u16 = 1;

/// 中身の暗号化の追加データの、用途の区切りの文字列。
const AAD_DOMAIN: &[u8] = b"nonoto/record/aad\0";

/// 書き込みの署名の対象の、用途の区切りの文字列。
const WRITE_SIGNATURE_DOMAIN: &[u8] = b"nonoto/record/write-signature\0";

/// トピック・ページ・枝のIDの長さ (バイト)。
pub(crate) const ID_LEN: usize = 16;

/// 封筒の種類の値。ログのレコードとスナップショットの値は、中身の鍵を導く用途にも使う。
const KIND_LOG: u8 = 1;
const KIND_SNAPSHOT: u8 = 2;
/// 世代の記録の封筒の種類の値。世代の記録はHPKEで包むため、中身の鍵の導出には使わない。
pub(crate) const KIND_GENERATION: u8 = 3;

/// 封筒の枝の値。
const BRANCH_BODY: u8 = 0;
const BRANCH_PROPOSAL: u8 = 1;

/// 中身の版と長さの部分の長さ (バイト)。
pub(crate) const CONTENT_HEADER_LEN: usize = 2 + 4;

macro_rules! random_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name([u8; $crate::record::ID_LEN]);

        impl $name {
            /// 乱数から新しいIDを作る。
            pub fn generate() -> Result<Self, $crate::error::Error> {
                let mut bytes = [0; $crate::record::ID_LEN];
                getrandom::fill(&mut bytes).map_err($crate::error::Error::Random)?;
                Ok(Self(bytes))
            }

            /// 封筒に置くバイト列からIDを作る。
            pub fn from_bytes(bytes: [u8; $crate::record::ID_LEN]) -> Self {
                Self(bytes)
            }

            /// 封筒に置くIDのバイト列を返す。
            pub fn as_bytes(&self) -> &[u8; $crate::record::ID_LEN] {
                &self.0
            }
        }
    };
}
pub(crate) use random_id;

random_id! {
    /// 同期で封筒に置くトピックのID。乱数にする (ADR #0006)。
    TopicId
}

random_id! {
    /// 同期で封筒に置くページのID。
    ///
    /// 端末のストアの [`crate::PageId`] は作成の時刻を含むため、封筒には置かず、別の乱数にする (ADR #0006)。
    SyncPageId
}

random_id! {
    /// 編集提案の枝のID。乱数にする (ADR #0009)。
    BranchId
}

/// レコードを書く先のログ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Branch {
    /// 本文の枝。
    Body,
    /// 編集提案の枝。
    Proposal(BranchId),
}

/// 書き込みの署名に必要な鍵の役割。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKeyRole {
    /// 本文とスナップショットの書き込みに使う鍵。
    Body,
    /// 編集提案のログの書き込みに使う鍵。
    Proposal,
}

/// レコードの種類。サーバーが仕事に要る区別だけにする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// ログのレコード。本文の更新と版の記録を、サーバーから見分けられない形で入れる (ADR #0009)。
    Log,
    /// 履歴をまとめたスナップショット。本文の書き込みの鍵で署名する。
    Snapshot {
        /// まとめた範囲の、ログの順番の上限。中継はこの順番までのレコードを消す。
        last_sequence: u64,
    },
}

impl RecordKind {
    fn value(self) -> u8 {
        match self {
            Self::Log => KIND_LOG,
            Self::Snapshot { .. } => KIND_SNAPSHOT,
        }
    }
}

/// 封筒に置く、サーバーに見せる値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    pub kind: RecordKind,
    pub suite: Suite,
    pub topic: TopicId,
    pub page: SyncPageId,
    pub branch: Branch,
    /// トピックの鍵と書き込みの鍵の世代の番号。
    pub generation: u64,
}

impl RecordHeader {
    /// この封筒の書き込みを許す鍵の役割を返す。
    ///
    /// ログは書く先の枝に従う。スナップショットには、枝を問わず本文の鍵を使う。
    pub fn write_key_role(&self) -> WriteKeyRole {
        match (self.kind, self.branch) {
            (RecordKind::Log, Branch::Proposal(_)) => WriteKeyRole::Proposal,
            (RecordKind::Log, Branch::Body) | (RecordKind::Snapshot { .. }, _) => {
                WriteKeyRole::Body
            }
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = encode_prefix(self.kind.value(), self.suite);
        bytes.extend_from_slice(self.topic.as_bytes());
        bytes.extend_from_slice(self.page.as_bytes());
        match self.branch {
            Branch::Body => bytes.push(BRANCH_BODY),
            Branch::Proposal(id) => {
                bytes.push(BRANCH_PROPOSAL);
                bytes.extend_from_slice(id.as_bytes());
            }
        }
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        if let RecordKind::Snapshot { last_sequence } = self.kind {
            bytes.extend_from_slice(&last_sequence.to_be_bytes());
        }
        bytes
    }

    /// 封筒を先頭から読み、封筒と読んだ長さを返す。
    ///
    /// 識別の印と封筒の版を最初に読み、知らない版なら残りを読まずに止める。
    fn decode(bytes: &[u8]) -> Result<(Self, usize), Error> {
        let mut reader = Reader::new(bytes);
        let (kind, suite) = decode_prefix(&mut reader)?;
        let topic = TopicId::from_bytes(reader.array()?);
        let page = SyncPageId::from_bytes(reader.array()?);
        let branch = match reader.u8()? {
            BRANCH_BODY => Branch::Body,
            BRANCH_PROPOSAL => Branch::Proposal(BranchId::from_bytes(reader.array()?)),
            _ => return Err(Error::MalformedRecord),
        };
        let generation = reader.u64()?;
        let kind = match kind {
            KIND_LOG => RecordKind::Log,
            KIND_SNAPSHOT => RecordKind::Snapshot {
                last_sequence: reader.u64()?,
            },
            _ => return Err(Error::MalformedRecord),
        };
        let header = Self {
            kind,
            suite,
            topic,
            page,
            branch,
            generation,
        };
        Ok((header, reader.position))
    }

    /// 中身の暗号化の追加データ。
    fn aad(encoded: &[u8]) -> Vec<u8> {
        [AAD_DOMAIN, encoded].concat()
    }
}

/// 暗号化して書き込みの鍵で署名したレコード。書き込みの署名はまだ確かめていない。
///
/// 中継は [`SealedRecord::parse`] で封筒を読み、封筒の値から選んだ公開鍵で [`SealedRecord::verify`] を呼ぶ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedRecord {
    bytes: Vec<u8>,
    header: RecordHeader,
    header_len: usize,
}

impl SealedRecord {
    /// レコードを暗号化し、書く先の書き込みの鍵で署名する。
    ///
    /// 中身の鍵は、トピックの鍵から、封筒の種類とページのIDを入れて導く。
    /// 書き込みの鍵は、封筒の世代と [`RecordHeader::write_key_role`] が返す役割に合わせる。
    pub fn seal(
        header: RecordHeader,
        payload: &[u8],
        topic_key: &TopicKey,
        write_key: &WriteSigningKey,
    ) -> Result<Self, Error> {
        Self::seal_with(header, payload, topic_key, write_key, CONTENT_VERSION, &[])
    }

    /// 中身の版と詰め物を指定して封じる。テストで、知らない中身の版と詰め物を作るのに使う。
    pub(crate) fn seal_with(
        header: RecordHeader,
        payload: &[u8],
        topic_key: &TopicKey,
        write_key: &WriteSigningKey,
        content_version: u16,
        padding: &[u8],
    ) -> Result<Self, Error> {
        let len = u32::try_from(payload.len()).map_err(|_| Error::RecordTooLarge)?;
        let mut contents = Zeroizing::new(Vec::with_capacity(
            CONTENT_HEADER_LEN + payload.len() + padding.len(),
        ));
        contents.extend_from_slice(&content_version.to_be_bytes());
        contents.extend_from_slice(&len.to_be_bytes());
        contents.extend_from_slice(payload);
        contents.extend_from_slice(padding);

        let mut bytes = header.encode();
        let header_len = bytes.len();
        let key =
            header
                .suite
                .derive_content_key(topic_key, header.kind.value(), header.page.as_bytes());
        let mut nonce = [0; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(Error::Random)?;
        let ciphertext =
            header
                .suite
                .encrypt(&key, &nonce, &RecordHeader::aad(&bytes), &contents)?;
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&ciphertext);
        let signature = write_key.sign(&signed_message(&bytes));
        bytes.extend_from_slice(&signature);
        Ok(Self {
            bytes,
            header,
            header_len,
        })
    }

    /// 受け取ったバイト列から、封筒を読む。
    ///
    /// 知らない封筒の版・方式のIDなら、それぞれ [`Error::UnsupportedEnvelopeVersion`]・
    /// [`Error::UnsupportedSuite`] を返す。
    pub fn parse(bytes: Vec<u8>) -> Result<Self, Error> {
        let (header, header_len) = RecordHeader::decode(&bytes)?;
        if bytes.len() < header_len + NONCE_LEN + TAG_LEN + CONTENT_HEADER_LEN + SIGNATURE_LEN {
            return Err(Error::MalformedRecord);
        }
        Ok(Self {
            bytes,
            header,
            header_len,
        })
    }

    /// 読み取った封筒を返す。この時点では署名を確かめていない。
    pub fn header(&self) -> &RecordHeader {
        &self.header
    }

    /// 封筒・nonce・暗号文・署名を含む、レコード全体のバイト列を借りる。
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 封筒・nonce・暗号文・署名を含む、レコード全体をバイト列として取り出す。
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// 書き込みの署名を、書く先の世代の書き込みの公開鍵で確かめる。
    ///
    /// 署名が合わなければ [`Error::InvalidWriteSignature`] を返す。
    /// 呼び出し側は封筒の世代と [`RecordHeader::write_key_role`] が返す役割に合わせて公開鍵を選ぶ。
    pub fn verify(self, key: &WriteVerifyingKey) -> Result<VerifiedRecord, Error> {
        let (signed, signature) = self.bytes.split_at(self.bytes.len() - SIGNATURE_LEN);
        let signature: &[u8; SIGNATURE_LEN] = signature
            .try_into()
            .expect("末尾から署名の長さだけ切り出したため、長さは署名の長さと一致する");
        key.verify(&signed_message(signed), signature)?;
        Ok(VerifiedRecord(self))
    }
}

/// 書き込みの署名を確かめたレコード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRecord(SealedRecord);

impl VerifiedRecord {
    /// 書き込みの署名を確かめた封筒を返す。
    pub fn header(&self) -> &RecordHeader {
        &self.0.header
    }

    /// 封筒・nonce・暗号文・署名を含む、署名を確かめたレコード全体のバイト列を借りる。
    pub fn as_bytes(&self) -> &[u8] {
        &self.0.bytes
    }

    /// 封筒・nonce・暗号文・署名を含む、署名を確かめたレコード全体をバイト列として取り出す。
    pub fn into_bytes(self) -> Vec<u8> {
        self.0.bytes
    }

    /// 中身を復号し、詰め物を除いた中身を返す。
    ///
    /// 鍵が違うか書き換えられていれば [`Error::DecryptRecord`]、
    /// 知らない中身の版なら [`Error::UnsupportedContentVersion`] を返す。
    pub fn open(&self, topic_key: &TopicKey) -> Result<RecordPayload, Error> {
        let record = &self.0;
        let header = &record.header;
        let encoded = &record.bytes[..record.header_len];
        let body = &record.bytes[record.header_len..record.bytes.len() - SIGNATURE_LEN];
        let (nonce, ciphertext) = body.split_at(NONCE_LEN);
        let nonce: &[u8; NONCE_LEN] = nonce
            .try_into()
            .expect("nonceの長さだけ切り出したため、長さはnonceの長さと一致する");
        let key =
            header
                .suite
                .derive_content_key(topic_key, header.kind.value(), header.page.as_bytes());
        let mut contents =
            header
                .suite
                .decrypt(&key, nonce, &RecordHeader::aad(encoded), ciphertext)?;

        let mut reader = Reader::new(&contents);
        let version = reader.u16()?;
        if version != CONTENT_VERSION {
            return Err(Error::UnsupportedContentVersion(version));
        }
        let len = usize::try_from(reader.u32()?).map_err(|_| Error::MalformedRecord)?;
        reader.take(len)?;
        let end = reader.position;

        // 版と長さを除いた中身だけを残し、詰め物と移動元に残ったバイトを消す。
        contents.copy_within(CONTENT_HEADER_LEN..end, 0);
        contents[len..].zeroize();
        contents.truncate(len);
        Ok(RecordPayload(contents))
    }
}

/// 復号したレコードの中身。破棄するとバイト列を消去する。
pub struct RecordPayload(Zeroizing<Vec<u8>>);

impl RecordPayload {
    /// 版・長さの欄と詰め物を除いた、中身のバイト列を借りる。
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// 中身の長さをバイト数で返す。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 中身が空なら真を返す。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for RecordPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordPayload")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

/// 封筒の先頭 (識別の印・封筒の版・種類・方式のID) を書く。どの種類の封筒も同じ形で始める。
pub(crate) fn encode_prefix(kind: u8, suite: Suite) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&ENVELOPE_VERSION.to_be_bytes());
    bytes.push(kind);
    bytes.extend_from_slice(&suite.id().to_be_bytes());
    bytes
}

/// 封筒の先頭を読み、種類の値と方式を返す。
///
/// 識別の印と封筒の版を最初に読み、知らない版なら残りを読まずに止める。
pub(crate) fn decode_prefix(reader: &mut Reader<'_>) -> Result<(u8, Suite), Error> {
    if reader.array::<4>().ok() != Some(MAGIC) {
        return Err(Error::UnknownRecordFormat);
    }
    let version = reader.u16()?;
    if version != ENVELOPE_VERSION {
        return Err(Error::UnsupportedEnvelopeVersion(version));
    }
    let kind = reader.u8()?;
    let suite = Suite::from_id(reader.u16()?)?;
    Ok((kind, suite))
}

/// 書き込みの署名の対象。区切りの文字列のあとに、封筒・nonce・暗号文を並べる。
fn signed_message(signed: &[u8]) -> Vec<u8> {
    [WRITE_SIGNATURE_DOMAIN, signed].concat()
}

/// バイト列を先頭から読む。足りなければ [`Error::MalformedRecord`] を返す。
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pub(crate) position: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(Error::MalformedRecord)?;
        let taken = &self.bytes[self.position..end];
        self.position = end;
        Ok(taken)
    }

    pub(crate) fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.position..]
    }

    pub(crate) fn array_ref<const N: usize>(&mut self) -> Result<&'a [u8; N], Error> {
        let taken = self.take(N)?;
        Ok(taken
            .try_into()
            .expect("Nバイトだけ切り出したため、長さはNと一致する"))
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        self.array_ref().copied()
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        self.array::<1>().map(|[value]| value)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        self.array().map(u16::from_be_bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        self.array().map(u32::from_be_bytes)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        self.array().map(u64::from_be_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, Update};

    struct Fixture {
        topic_key: TopicKey,
        body_signer: WriteSigningKey,
        proposal_signer: WriteSigningKey,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                topic_key: TopicKey::generate().expect("トピックの鍵を作れなかった"),
                body_signer: WriteSigningKey::generate().expect("本文の書き込みの鍵を作れなかった"),
                proposal_signer: WriteSigningKey::generate()
                    .expect("提案の書き込みの鍵を作れなかった"),
            }
        }

        fn write_key(&self, header: &RecordHeader) -> &WriteSigningKey {
            match header.write_key_role() {
                WriteKeyRole::Body => &self.body_signer,
                WriteKeyRole::Proposal => &self.proposal_signer,
            }
        }

        fn seal(&self, header: RecordHeader, payload: &[u8]) -> SealedRecord {
            SealedRecord::seal(header, payload, &self.topic_key, self.write_key(&header))
                .expect("レコードを封じられなかった")
        }

        fn verify(&self, record: SealedRecord) -> Result<VerifiedRecord, Error> {
            let key = self.write_key(record.header()).verifying_key();
            record.verify(&key)
        }

        /// 受け取ったバイト列を、封筒の読み取り・書き込みの署名の確かめ・復号の順に通す。
        fn receive(&self, bytes: Vec<u8>) -> Result<RecordPayload, Error> {
            self.verify(SealedRecord::parse(bytes)?)?
                .open(&self.topic_key)
        }
    }

    fn header(kind: RecordKind, branch: Branch) -> RecordHeader {
        RecordHeader {
            kind,
            suite: Suite::CURRENT,
            topic: TopicId::generate().expect("トピックのIDを作れなかった"),
            page: SyncPageId::generate().expect("ページのIDを作れなかった"),
            branch,
            generation: 3,
        }
    }

    fn log_header() -> RecordHeader {
        header(RecordKind::Log, Branch::Body)
    }

    fn headers() -> Vec<RecordHeader> {
        let proposal = Branch::Proposal(BranchId::generate().expect("枝のIDを作れなかった"));
        let snapshot = RecordKind::Snapshot { last_sequence: 42 };
        vec![
            header(RecordKind::Log, Branch::Body),
            header(RecordKind::Log, proposal),
            header(snapshot, Branch::Body),
            header(snapshot, proposal),
        ]
    }

    /// 封筒の中で、指定した値が置かれた位置。
    fn offset_of(record: &SealedRecord, needle: &[u8]) -> usize {
        record.as_bytes()[..record.header_len]
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("封筒の中に値が見つからなかった")
    }

    /// 受け取ったときのエラーが期待したものかを確かめる関数。
    type IsExpected = fn(&Error) -> bool;

    fn flipped(bytes: &[u8], index: usize) -> Vec<u8> {
        let mut bytes = bytes.to_vec();
        bytes[index] ^= 0x01;
        bytes
    }

    #[test]
    fn 版1の封筒は固定のバイト列と互換である() {
        let bytes = [
            b"NNTR\x00\x01\x02\x00\x01".as_slice(),
            b"0123456789abcdef",
            b"fedcba9876543210",
            b"\x01abcdefghijklmnop",
            b"\x01\x02\x03\x04\x05\x06\x07\x08",
            b"\x11\x12\x13\x14\x15\x16\x17\x18",
        ]
        .concat();
        let expected = RecordHeader {
            kind: RecordKind::Snapshot {
                last_sequence: 0x1112_1314_1516_1718,
            },
            suite: Suite::XWingEd25519XChaCha20,
            topic: TopicId::from_bytes(*b"0123456789abcdef"),
            page: SyncPageId::from_bytes(*b"fedcba9876543210"),
            branch: Branch::Proposal(BranchId::from_bytes(*b"abcdefghijklmnop")),
            generation: 0x0102_0304_0506_0708,
        };

        let (header, len) = RecordHeader::decode(&bytes).expect("版1の封筒を読めなかった");
        assert_eq!(header, expected, "版1の封筒から読み取った値が違う");
        assert_eq!(len, bytes.len(), "読み取った封筒の長さが違う");
        assert_eq!(
            expected.encode(),
            bytes,
            "書き出した版1の封筒のバイト列が違う"
        );
    }

    #[test]
    fn 版1のログのレコードを固定のバイト列から確かめて開ける() {
        // 書き込みの鍵の種を0x11の32バイト、トピックの鍵を0x5aの32バイトにして、
        // 本文の枝のログに `payload` を封じて作った版1のレコード。
        // 形式を変えたときに、すでに作ったレコードを読めなくなることを見つけるため、作り直さない。
        let bytes = [
            b"NNTR\x00\x01\x01\x00\x01".as_slice(),
            b"0123456789abcdef",
            b"fedcba9876543210",
            b"\x00",
            b"\x01\x02\x03\x04\x05\x06\x07\x08",
            // nonce
            &[
                0x60, 0xa8, 0x26, 0xfe, 0xa5, 0x14, 0x3a, 0x57, 0xcf, 0x67, 0xd9, 0xe5, 0xe5, 0x89,
                0x45, 0xd3, 0x55, 0x99, 0x36, 0xcf, 0x40, 0x43, 0x0c, 0xea,
            ],
            // 暗号文 (中身の版・中身の長さ・中身と、認証タグ)
            &[
                0x80, 0xf0, 0x98, 0x1d, 0xaa, 0xdf, 0x79, 0x40, 0x3d, 0x53, 0xbe, 0x01, 0x7a, 0x02,
                0x1d, 0xaf, 0xbb, 0xcb, 0x3c, 0x19, 0xa7, 0x6c, 0x7f, 0xb4, 0x55, 0xb4, 0x94, 0x69,
                0x1c,
            ],
            // 書き込みの署名
            &[
                0x64, 0x1d, 0x09, 0x27, 0x24, 0xe2, 0x07, 0xb4, 0x58, 0x30, 0x5e, 0x6c, 0xa5, 0x77,
                0xb3, 0x10, 0xc4, 0x11, 0x76, 0xf4, 0x7a, 0xa4, 0xe5, 0x14, 0xf6, 0xf9, 0x18, 0xe9,
                0x12, 0x11, 0xc0, 0x6c, 0x7e, 0xb1, 0x8b, 0x81, 0x77, 0x60, 0x00, 0xd2, 0x66, 0xa9,
                0x44, 0x57, 0x2c, 0xe8, 0x38, 0xd7, 0x76, 0x1e, 0x1a, 0x75, 0x17, 0x9e, 0x53, 0x1c,
                0x18, 0x37, 0x36, 0x71, 0x5f, 0x8e, 0x76, 0x0e,
            ],
        ]
        .concat();
        let write_key = WriteVerifyingKey::from_bytes(&[
            0xd0, 0x4a, 0xb2, 0x32, 0x74, 0x2b, 0xb4, 0xab, 0x3a, 0x13, 0x68, 0xbd, 0x46, 0x15,
            0xe4, 0xe6, 0xd0, 0x22, 0x4a, 0xb7, 0x1a, 0x01, 0x6b, 0xaf, 0x85, 0x20, 0xa3, 0x32,
            0xc9, 0x77, 0x87, 0x37,
        ])
        .expect("書き込みの公開鍵を読めなかった");
        let topic_key = TopicKey::from_bytes(&[0x5a; 32]);
        let expected = RecordHeader {
            kind: RecordKind::Log,
            suite: Suite::XWingEd25519XChaCha20,
            topic: TopicId::from_bytes(*b"0123456789abcdef"),
            page: SyncPageId::from_bytes(*b"fedcba9876543210"),
            branch: Branch::Body,
            generation: 0x0102_0304_0506_0708,
        };

        let record = SealedRecord::parse(bytes).expect("版1のレコードの封筒を読めなかった");
        assert_eq!(
            *record.header(),
            expected,
            "版1のレコードから読み取った封筒の値が違う"
        );
        let payload = record
            .verify(&write_key)
            .expect("版1のレコードの書き込みの署名が合わない")
            .open(&topic_key)
            .expect("版1のレコードを開けなかった");
        assert_eq!(
            payload.as_bytes(),
            b"payload",
            "版1のレコードから開いた中身が違う"
        );
    }

    #[test]
    fn 封じたレコードを確かめて開ける() {
        let fixture = Fixture::new();
        for header in headers() {
            let record = fixture.seal(header, b"payload");
            let parsed = SealedRecord::parse(record.into_bytes()).expect("封筒を読めなかった");
            assert_eq!(*parsed.header(), header, "読んだ封筒が封じた封筒と異なる");

            let payload = fixture
                .receive(parsed.into_bytes())
                .expect("レコードを開けなかった");
            assert_eq!(
                payload.as_bytes(),
                b"payload",
                "開いた中身が封じた中身と異なる"
            );
        }
    }

    #[test]
    fn 封じた文書の更新を開いて別の複製に当てられる() {
        let fixture = Fixture::new();
        let mut local = Document::new("").expect("文書を作れなかった");
        let snapshot = local
            .snapshot()
            .expect("スナップショットを書き出せなかった");
        let update = local
            .apply_local(0..0, "こんにちは")
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わったのに更新が無い");

        let record = fixture.seal(log_header(), update.as_bytes());
        let payload = fixture
            .receive(record.into_bytes())
            .expect("レコードを開けなかった");
        let mut remote = Document::from_snapshot(&snapshot).expect("文書を開けなかった");
        remote
            .apply_remote(&Update::from_bytes(payload.as_bytes().to_vec()))
            .expect("開いた更新を当てられなかった");

        assert_eq!(
            remote.text(),
            "こんにちは",
            "更新を当てた本文が手元と異なる"
        );
    }

    #[test]
    fn レコードのどのバイトを書き換えても拒む() {
        let fixture = Fixture::new();
        for header in headers() {
            let bytes = fixture.seal(header, b"payload").into_bytes();
            for index in 0..bytes.len() {
                assert!(
                    fixture.receive(flipped(&bytes, index)).is_err(),
                    "{index}バイト目を書き換えたレコードを受け付けた ({header:?})",
                );
            }
        }
    }

    #[test]
    fn 封筒の値を書き換えたレコードは書き込みの署名が合わない() {
        let fixture = Fixture::new();
        let proposal = BranchId::generate().expect("枝のIDを作れなかった");
        let header = header(
            RecordKind::Snapshot { last_sequence: 42 },
            Branch::Proposal(proposal),
        );
        let record = fixture.seal(header, b"payload");
        let fields: [(&str, &[u8]); 5] = [
            ("トピックのID", header.topic.as_bytes()),
            ("ページのID", header.page.as_bytes()),
            ("枝のID", proposal.as_bytes()),
            ("世代の番号", &header.generation.to_be_bytes()),
            ("まとめた順番の上限", &42u64.to_be_bytes()),
        ];

        for (name, value) in fields {
            let index = offset_of(&record, value);
            let result = SealedRecord::parse(flipped(record.as_bytes(), index))
                .expect("書き換えた値の封筒を読めなかった")
                .verify(&fixture.body_signer.verifying_key());
            assert!(
                matches!(result, Err(Error::InvalidWriteSignature)),
                "{name}を書き換えたレコードの署名を受け付けた",
            );
        }
    }

    #[test]
    fn 導出される鍵が同じでも封筒の追加データが違えば開けない() {
        let fixture = Fixture::new();
        let original = RecordHeader {
            topic: TopicId::from_bytes([1; 16]),
            ..header(
                RecordKind::Log,
                Branch::Proposal(BranchId::from_bytes([1; 16])),
            )
        };
        let record = fixture.seal(original, b"payload");
        let changed_headers = [
            RecordHeader {
                generation: original.generation + 1,
                ..original
            },
            RecordHeader {
                topic: TopicId::from_bytes([2; 16]),
                ..original
            },
            RecordHeader {
                branch: Branch::Proposal(BranchId::from_bytes([3; 16])),
                ..original
            },
        ];
        let body = &record.as_bytes()[record.header_len..record.as_bytes().len() - SIGNATURE_LEN];

        for changed in changed_headers {
            let mut bytes = changed.encode();
            bytes.extend_from_slice(body);
            let signature = fixture.write_key(&changed).sign(&signed_message(&bytes));
            bytes.extend_from_slice(&signature);
            let parsed = SealedRecord::parse(bytes).expect("封筒を読めなかった");
            let verified = fixture.verify(parsed).expect("書き直した署名が合わない");

            assert!(
                matches!(verified.open(&fixture.topic_key), Err(Error::DecryptRecord)),
                "追加データが合わないレコードを開けた: {changed:?}",
            );
        }
    }

    #[test]
    fn スナップショットには本文の書き込み鍵を使う() {
        let fixture = Fixture::new();
        let proposal = Branch::Proposal(BranchId::from_bytes([1; 16]));
        let snapshot = RecordKind::Snapshot { last_sequence: 42 };
        for (kind, branch, allowed, rejected) in [
            (
                RecordKind::Log,
                Branch::Body,
                &fixture.body_signer,
                &fixture.proposal_signer,
            ),
            (
                RecordKind::Log,
                proposal,
                &fixture.proposal_signer,
                &fixture.body_signer,
            ),
            (
                snapshot,
                Branch::Body,
                &fixture.body_signer,
                &fixture.proposal_signer,
            ),
            (
                snapshot,
                proposal,
                &fixture.body_signer,
                &fixture.proposal_signer,
            ),
        ] {
            let header = header(kind, branch);
            let signed = SealedRecord::seal(header, b"payload", &fixture.topic_key, allowed)
                .expect("レコードを封じられなかった");
            let payload = fixture
                .receive(signed.into_bytes())
                .expect("要求された書き込み鍵の署名を拒んだ");
            assert_eq!(payload.as_bytes(), b"payload", "開いた中身が違う");

            let signed = SealedRecord::seal(header, b"payload", &fixture.topic_key, rejected)
                .expect("テスト用のレコードを封じられなかった");
            assert!(
                matches!(
                    fixture.receive(signed.into_bytes()),
                    Err(Error::InvalidWriteSignature)
                ),
                "要求された役割と違う鍵の署名を受け付けた: {kind:?} {branch:?}",
            );
        }
    }

    #[test]
    fn 別の書き込みの鍵で署名したレコードを拒む() {
        let fixture = Fixture::new();
        let other = WriteSigningKey::generate().expect("書き込みの鍵を作れなかった");
        let record = SealedRecord::seal(log_header(), b"payload", &fixture.topic_key, &other)
            .expect("レコードを封じられなかった");

        assert!(
            matches!(
                fixture.receive(record.into_bytes()),
                Err(Error::InvalidWriteSignature)
            ),
            "別の書き込みの鍵で署名したレコードを受け付けた",
        );
    }

    #[test]
    fn 別のトピックの鍵では開けない() {
        let fixture = Fixture::new();
        let record = fixture
            .seal(log_header(), b"payload")
            .verify(&fixture.body_signer.verifying_key())
            .expect("書き込みの署名が合わなかった");
        let other = TopicKey::generate().expect("トピックの鍵を作れなかった");

        assert!(
            matches!(record.open(&other), Err(Error::DecryptRecord)),
            "別のトピックの鍵で開けた",
        );
    }

    #[test]
    fn 知らない封筒の版と方式と中身の版を_文書を変えずに止める() {
        let fixture = Fixture::new();
        let mut local = Document::new("").expect("文書を作れなかった");
        let mut remote = Document::from_snapshot(
            &local
                .snapshot()
                .expect("スナップショットを書き出せなかった"),
        )
        .expect("文書を開けなかった");
        let update = local
            .apply_local(0..0, "本文")
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わったのに更新が無い");
        let bytes = fixture.seal(log_header(), update.as_bytes()).into_bytes();

        let mut unknown_envelope = bytes.clone();
        unknown_envelope[MAGIC.len()..MAGIC.len() + 2].copy_from_slice(&2u16.to_be_bytes());
        let mut unknown_suite = bytes.clone();
        unknown_suite[MAGIC.len() + 3..MAGIC.len() + 5].copy_from_slice(&0xffffu16.to_be_bytes());
        let unknown_content = SealedRecord::seal_with(
            log_header(),
            update.as_bytes(),
            &fixture.topic_key,
            &fixture.body_signer,
            2,
            &[],
        )
        .expect("レコードを封じられなかった")
        .into_bytes();

        let cases: [(Vec<u8>, &str, IsExpected); 3] = [
            (unknown_envelope, "封筒の版", |err| {
                matches!(err, Error::UnsupportedEnvelopeVersion(2))
            }),
            (unknown_suite, "方式のID", |err| {
                matches!(err, Error::UnsupportedSuite(0xffff))
            }),
            (unknown_content, "中身の版", |err| {
                matches!(err, Error::UnsupportedContentVersion(2))
            }),
        ];
        for (bytes, name, is_expected) in cases {
            let result = fixture.receive(bytes).and_then(|payload| {
                remote.apply_remote(&Update::from_bytes(payload.as_bytes().to_vec()))
            });
            let expected = result.as_ref().is_err_and(is_expected);
            assert!(
                expected,
                "知らない{name}のレコードで、期待したエラーにならなかった: {result:?}"
            );
            assert_eq!(
                remote.text(),
                "",
                "知らない{name}のレコードで文書が変わった"
            );
        }
    }

    #[test]
    fn 識別の印が違うバイト列を拒む() {
        let fixture = Fixture::new();
        let bytes = fixture.seal(log_header(), b"payload").into_bytes();

        assert!(
            matches!(
                SealedRecord::parse(flipped(&bytes, 0)),
                Err(Error::UnknownRecordFormat)
            ),
            "識別の印が違うバイト列を受け付けた",
        );
    }

    #[test]
    fn 識別の印のあとで切れたバイト列を形式の誤りとして拒む() {
        for bytes in [b"NNTR".as_slice(), b"NNTR\x00"] {
            assert!(
                matches!(
                    SealedRecord::parse(bytes.to_vec()),
                    Err(Error::MalformedRecord)
                ),
                "識別の印のあとで切れたバイト列を、形式の誤りとして拒まなかった: {bytes:?}",
            );
        }
    }

    #[test]
    fn 詰め物を足したレコードを読める() {
        let fixture = Fixture::new();
        for (expected, padding) in [
            (b"payload".as_slice(), vec![0; 1]),
            (b"payload".as_slice(), vec![0; 100]),
            (b"payload".as_slice(), vec![0xff; 4096]),
            (b"".as_slice(), vec![0xff; 1]),
        ] {
            let record = SealedRecord::seal_with(
                log_header(),
                expected,
                &fixture.topic_key,
                &fixture.body_signer,
                CONTENT_VERSION,
                &padding,
            )
            .expect("レコードを封じられなかった");

            let payload = fixture
                .receive(record.into_bytes())
                .expect("詰め物を足したレコードを開けなかった");
            assert_eq!(
                payload.as_bytes(),
                expected,
                "詰め物を足したレコードの中身が封じた中身と異なる"
            );
            assert_eq!(
                payload.len(),
                expected.len(),
                "長さに中身以外のバイトを含めた"
            );
            assert_eq!(
                payload.is_empty(),
                expected.is_empty(),
                "中身が空かどうかの判定が違う"
            );
        }
    }

    #[test]
    fn 復号した中身をデバッグ表示で伏せる() {
        let fixture = Fixture::new();
        let bytes = fixture.seal(log_header(), b"payload").into_bytes();
        let payload = fixture.receive(bytes).expect("レコードを開けなかった");
        assert_eq!(
            format!("{payload:?}"),
            "RecordPayload { len: 7, .. }",
            "復号した中身のデバッグ表示に長さ以外の値を含めた",
        );
    }

    #[test]
    fn 途中で切れたレコードを拒む() {
        let fixture = Fixture::new();
        for header in headers() {
            let bytes = fixture.seal(header, b"payload").into_bytes();
            for len in 0..bytes.len() {
                assert!(
                    fixture.receive(bytes[..len].to_vec()).is_err(),
                    "{len}バイトで切れたレコードを受け付けた ({header:?})",
                );
            }
        }
    }
}
