//! 世代の記録。世代ごとのトピックの鍵と書き込みの鍵を作り、メンバーの役割に応じて包んで配る。
//!
//! ```text
//! 封筒: 識別の印 (4) | 封筒の版 (2) | 種類 (1) | 方式のID (2) | トピックのID (16) | 世代の番号 (8)
//!       | 提案の書き込みの公開鍵 (32) | 本文の書き込みの公開鍵 (32)
//! 包み: 包みの数 (2) | 包みごとに、宛先のメンバーのID (16) | カプセルの長さ (4) | カプセル
//!       | 暗号文の長さ (4) | 暗号文
//! 包みの中身: 中身の版 (2) | 中身の長さ (4) | 中身 | 詰め物
//! 中身: 作ったメンバーのID (16) | 作ったメンバーの署名 (64) | 役割 (1) | トピックの鍵 (32)
//!       | (提案と編集だけ) 提案の書き込みの鍵 (32) | (編集だけ) 本文の書き込みの鍵 (32)
//! ```
//!
//! 包みはメンバーの利用者の公開鍵でHPKEで包み、infoと追加データに、トピックのID・世代の番号・宛先を入れる。
//! 作ったメンバーは、封筒・宛先・役割・宛先に渡す鍵に署名する。署名は包みの中に入れる。
//! サーバーには、誰が作ったかを見せない (ADR #0009)。

use std::collections::HashSet;
use std::fmt;

use zeroize::Zeroizing;

use crate::crypto::{
    ED25519_KEY_LEN, SIGNATURE_LEN, Suite, TOPIC_KEY_LEN, TopicKey, UserSigningKey, Wrapped,
    WriteSigningKey, WriteVerifyingKey,
};
use crate::error::Error;
use crate::member::{Member, MemberId, Membership, Role, UserKeys};
use crate::record::{
    CONTENT_HEADER_LEN, ID_LEN, KIND_GENERATION, Reader, TopicId, decode_prefix, encode_prefix,
};

/// 世代の包みの中身の版。作成者の署名で、宛先と役割と配布する鍵まで認証する。
const WRAP_CONTENT_VERSION: u16 = 1;

/// 作成者・署名・役割・トピックの鍵の長さ。書き込みの鍵の種はこの後に続く。
const MIN_WRAP_CONTENT_LEN: usize = ID_LEN + SIGNATURE_LEN + 1 + TOPIC_KEY_LEN;

/// 包みのinfoの、用途の区切りの文字列。
const WRAP_INFO_DOMAIN: &[u8] = b"nonoto/generation/wrap-info\0";

/// 包みの追加データの、用途の区切りの文字列。
const WRAP_AAD_DOMAIN: &[u8] = b"nonoto/generation/wrap-aad\0";

/// 作ったメンバーの署名の対象の、用途の区切りの文字列。
const CREATOR_SIGNATURE_DOMAIN: &[u8] = b"nonoto/generation/creator-signature\0";

/// 世代ごとのトピックの鍵と、提案と本文の書き込みの鍵の組。新しい世代を作る編集の役割のメンバーが作る。
///
/// ログに出さないよう、`Debug` では秘密鍵を表示しない。
#[derive(Clone)]
pub struct GenerationKeys {
    topic: TopicKey,
    proposal: WriteSigningKey,
    body: WriteSigningKey,
}

impl GenerationKeys {
    /// 乱数から、新しい世代の鍵を作る。
    pub fn generate() -> Result<Self, Error> {
        Ok(Self {
            topic: TopicKey::generate()?,
            proposal: WriteSigningKey::generate()?,
            body: WriteSigningKey::generate()?,
        })
    }

    /// 世代のトピックの鍵。世代を作ったメンバーが、そのまま書くのに使う。
    pub fn topic_key(&self) -> &TopicKey {
        &self.topic
    }

    /// 提案の書き込みの鍵。
    pub fn proposal_key(&self) -> &WriteSigningKey {
        &self.proposal
    }

    /// 本文の書き込みの鍵。
    pub fn body_key(&self) -> &WriteSigningKey {
        &self.body
    }

    /// 世代の記録の封筒に置く、書き込みの公開鍵の組を返す。
    pub fn write_keys(&self) -> WriteKeys {
        WriteKeys {
            proposal: self.proposal.verifying_key(),
            body: self.body.verifying_key(),
        }
    }
}

impl fmt::Debug for GenerationKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GenerationKeys(..)")
    }
}

/// 世代の書き込みの公開鍵の組。中継は、これで書き込みの署名を確かめる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteKeys {
    /// 提案の書き込みの公開鍵。
    pub proposal: WriteVerifyingKey,
    /// 本文の書き込みの公開鍵。
    pub body: WriteVerifyingKey,
}

/// 世代の記録の封筒に置く、サーバーに見せる値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationHeader {
    pub suite: Suite,
    pub topic: TopicId,
    /// 世代の番号。
    pub generation: u64,
    pub write_keys: WriteKeys,
}

impl GenerationHeader {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = encode_prefix(KIND_GENERATION, self.suite);
        bytes.extend_from_slice(self.topic.as_bytes());
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        bytes.extend_from_slice(&self.write_keys.proposal.to_bytes());
        bytes.extend_from_slice(&self.write_keys.body.to_bytes());
        bytes
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let (kind, suite) = decode_prefix(reader)?;
        if kind != KIND_GENERATION {
            return Err(Error::MalformedRecord);
        }
        let topic = TopicId::from_bytes(reader.array()?);
        let generation = reader.u64()?;
        let proposal =
            WriteVerifyingKey::from_bytes(&reader.array()?).map_err(|_| Error::MalformedRecord)?;
        let body =
            WriteVerifyingKey::from_bytes(&reader.array()?).map_err(|_| Error::MalformedRecord)?;
        Ok(Self {
            suite,
            topic,
            generation,
            write_keys: WriteKeys { proposal, body },
        })
    }

    /// 宛先ごとの包みのinfo。
    fn wrap_info(&self, recipient: &MemberId) -> Vec<u8> {
        let mut info = WRAP_INFO_DOMAIN.to_vec();
        info.extend_from_slice(self.topic.as_bytes());
        info.extend_from_slice(&self.generation.to_be_bytes());
        info.extend_from_slice(recipient.as_bytes());
        info
    }
}

/// 署名に使う、作成者と宛先ごとの配布内容。鍵を含むため、使い終わったら消す。
fn creator_message(
    encoded_header: &[u8],
    creator: MemberId,
    recipient: MemberId,
    role_and_keys: &[u8],
) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(
        [
            CREATOR_SIGNATURE_DOMAIN,
            &WRAP_CONTENT_VERSION.to_be_bytes(),
            encoded_header,
            creator.as_bytes(),
            recipient.as_bytes(),
            role_and_keys,
        ]
        .concat(),
    )
}

/// 宛先ごとの包みの追加データ。封筒の全体と宛先を入れる。
fn wrap_aad(encoded_header: &[u8], recipient: &MemberId) -> Vec<u8> {
    [WRAP_AAD_DOMAIN, encoded_header, recipient.as_bytes()].concat()
}

/// 包みを開いて受け取った、自分の役割に応じた鍵。
///
/// 閲覧の役割はトピックの鍵だけ、提案の役割は提案の書き込みの鍵まで、編集の役割は本文の書き込みの鍵までを持つ。
#[derive(Debug, Clone)]
pub struct RoleKeys {
    role: Role,
    topic: TopicKey,
    proposal: Option<WriteSigningKey>,
    body: Option<WriteSigningKey>,
}

impl RoleKeys {
    /// 受け手のメンバー構成と一致することを確かめた役割。
    pub fn role(&self) -> Role {
        self.role
    }

    /// 作成者の署名を確かめた、世代のトピックの鍵を借りる。
    pub fn topic_key(&self) -> &TopicKey {
        &self.topic
    }

    /// 提案の書き込みの鍵。閲覧の役割では `None` を返す。
    pub fn proposal_key(&self) -> Option<&WriteSigningKey> {
        self.proposal.as_ref()
    }

    /// 本文の書き込みの鍵。編集の役割でなければ `None` を返す。
    pub fn body_key(&self) -> Option<&WriteSigningKey> {
        self.body.as_ref()
    }
}

/// 世代の記録。新しい世代を作った編集の役割のメンバーが、メンバーごとの包みをまとめて送る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRecord {
    bytes: Vec<u8>,
    header: GenerationHeader,
    header_len: usize,
    wraps: Vec<(MemberId, Wrapped)>,
}

impl GenerationRecord {
    /// 世代の鍵を、メンバー構成の全員に役割に応じて包み、作ったメンバーの署名を入れた世代の記録を作る。
    ///
    /// 封筒の書き込みの公開鍵は、世代の鍵から組む。
    /// 作ったメンバーがメンバー構成に無いか、編集の役割でなければ [`Error::CreatorNotEditor`] を返す。
    /// 署名鍵の公開鍵がメンバー構成と違えば [`Error::CreatorKeyMismatch`] を返す。
    pub fn create(
        suite: Suite,
        topic: TopicId,
        generation: u64,
        keys: &GenerationKeys,
        creator: MemberId,
        creator_keys: &UserKeys,
        membership: &Membership,
    ) -> Result<Self, Error> {
        let header = GenerationHeader {
            suite,
            topic,
            generation,
            write_keys: keys.write_keys(),
        };
        let creator_member = membership
            .get(&creator)
            .filter(|member| member.role == Role::Editor)
            .ok_or(Error::CreatorNotEditor)?;
        let signing = creator_keys.signing();
        if signing.verifying_key() != creator_member.keys.signing {
            return Err(Error::CreatorKeyMismatch);
        }

        let members = membership.iter();
        let count = u16::try_from(members.len()).map_err(|_| Error::RecordTooLarge)?;
        let encoded = header.encode();
        let mut wraps = Vec::with_capacity(usize::from(count));
        for member in members {
            let contents = wrap_contents(&encoded, creator, signing, member, keys)?;
            let wrapped = header.suite.wrap(
                &member.keys.wrap,
                &header.wrap_info(&member.id),
                &wrap_aad(&encoded, &member.id),
                &contents,
            )?;
            wraps.push((member.id, wrapped));
        }

        let mut bytes = encoded;
        let header_len = bytes.len();
        bytes.extend_from_slice(&count.to_be_bytes());
        for (recipient, wrapped) in &wraps {
            bytes.extend_from_slice(recipient.as_bytes());
            for part in [&wrapped.enc, &wrapped.ciphertext] {
                let len = u32::try_from(part.len()).map_err(|_| Error::RecordTooLarge)?;
                bytes.extend_from_slice(&len.to_be_bytes());
                bytes.extend_from_slice(part);
            }
        }
        Ok(Self {
            bytes,
            header,
            header_len,
            wraps,
        })
    }

    /// 受け取ったバイト列から、封筒と包みの並びを読む。
    ///
    /// 包みが無いか、同じ宛先の包みが2つあるか、包みのカプセルと暗号文が方式で開ける長さでないか、
    /// 読み終えたあとにバイトが残れば [`Error::MalformedRecord`] を返す。
    pub fn parse(bytes: Vec<u8>) -> Result<Self, Error> {
        let mut reader = Reader::new(&bytes);
        let header = GenerationHeader::decode(&mut reader)?;
        let header_len = reader.position;
        let count = usize::from(reader.u16()?);
        let (enc_len, tag_len) = header.suite.wrap_lengths();
        let min_ciphertext_len = CONTENT_HEADER_LEN + MIN_WRAP_CONTENT_LEN + tag_len;
        let min_wrap_len = ID_LEN + 2 * size_of::<u32>() + enc_len + min_ciphertext_len;
        if count == 0 || count > reader.remaining().len() / min_wrap_len {
            return Err(Error::MalformedRecord);
        }

        let mut recipients = HashSet::with_capacity(count);
        let mut wraps = Vec::with_capacity(count);
        for _ in 0..count {
            let recipient = MemberId::from_bytes(reader.array()?);
            if !recipients.insert(recipient) {
                return Err(Error::MalformedRecord);
            }
            let enc = read_part(&mut reader)?;
            let ciphertext = read_part(&mut reader)?;
            if enc.len() != enc_len || ciphertext.len() < min_ciphertext_len {
                return Err(Error::MalformedRecord);
            }
            wraps.push((
                recipient,
                Wrapped {
                    enc: enc.to_vec(),
                    ciphertext: ciphertext.to_vec(),
                },
            ));
        }
        if reader.position != bytes.len() {
            return Err(Error::MalformedRecord);
        }
        Ok(Self {
            bytes,
            header,
            header_len,
            wraps,
        })
    }

    /// 封筒の値を返す。中継は、世代の番号と書き込みの公開鍵をここから読む。
    pub fn header(&self) -> &GenerationHeader {
        &self.header
    }

    /// 包みの宛先のメンバーを返す。
    pub fn recipients(&self) -> impl Iterator<Item = &MemberId> {
        self.wraps.iter().map(|(recipient, _)| recipient)
    }

    /// 封筒と暗号化された包みを含む、記録全体のバイト列を借りる。
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 封筒と暗号化された包みを含む、記録全体をバイト列として取り出す。
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// 自分宛ての包みを開き、作ったメンバーの署名と役割を確かめて、自分の役割に応じた鍵を返す。
    ///
    /// 次のときはエラーを返し、鍵を返さない。
    ///
    /// - 自分がメンバー構成に無い ([`Error::UnknownMember`])、自分宛ての包みが無い ([`Error::NotRecipient`])
    /// - 包みを開けない。ほかの宛先や世代から付け替えた包みもここで止まる ([`Error::DecryptRecord`])
    /// - 作ったメンバーが編集の役割でない ([`Error::CreatorNotEditor`])、署名が合わない ([`Error::InvalidCreatorSignature`])
    /// - 包みの中の役割がメンバー構成の自分の役割と違うか、書き込みの鍵が封筒の公開鍵と合わない ([`Error::WrappedKeysMismatch`])
    pub fn open(
        &self,
        me: MemberId,
        my_keys: &UserKeys,
        membership: &Membership,
    ) -> Result<RoleKeys, Error> {
        let my_role = membership.get(&me).ok_or(Error::UnknownMember)?.role;
        let (_, wrapped) = self
            .wraps
            .iter()
            .find(|(recipient, _)| *recipient == me)
            .ok_or(Error::NotRecipient)?;
        let header = &self.header;
        let encoded = &self.bytes[..self.header_len];
        let contents = header.suite.unwrap(
            my_keys.wrap_secret(),
            wrapped,
            &header.wrap_info(&me),
            &wrap_aad(encoded, &me),
        )?;

        let mut outer = Reader::new(&contents);
        let version = outer.u16()?;
        if version != WRAP_CONTENT_VERSION {
            return Err(Error::UnsupportedContentVersion(version));
        }
        let len = usize::try_from(outer.u32()?).map_err(|_| Error::MalformedRecord)?;
        // 中身の長さのあとは詰め物のため、読み飛ばす。
        let mut reader = Reader::new(outer.take(len)?);

        let creator = MemberId::from_bytes(reader.array()?);
        let signature = reader.array_ref::<SIGNATURE_LEN>()?;
        let role_and_keys = reader.remaining();
        let role = Role::from_value(reader.u8()?)?;
        if len != wrap_content_len(role) {
            return Err(Error::MalformedRecord);
        }
        let creator_member = membership
            .get(&creator)
            .filter(|member| member.role == Role::Editor)
            .ok_or(Error::CreatorNotEditor)?;
        let signed = creator_message(encoded, creator, me, role_and_keys);
        creator_member
            .keys
            .signing
            .verify_creator(&signed, signature)?;
        if role != my_role {
            return Err(Error::WrappedKeysMismatch);
        }

        let topic = TopicKey::from_bytes(reader.array_ref()?);
        let proposal = (role >= Role::Proposer)
            .then(|| read_write_key(&mut reader))
            .transpose()?;
        let body = (role == Role::Editor)
            .then(|| read_write_key(&mut reader))
            .transpose()?;
        let proposal_matches = proposal
            .as_ref()
            .is_none_or(|key| key.verifying_key() == header.write_keys.proposal);
        let body_matches = body
            .as_ref()
            .is_none_or(|key| key.verifying_key() == header.write_keys.body);
        if !proposal_matches || !body_matches {
            return Err(Error::WrappedKeysMismatch);
        }
        Ok(RoleKeys {
            role,
            topic,
            proposal,
            body,
        })
    }
}

fn wrap_content_len(role: Role) -> usize {
    let write_key_count = match role {
        Role::Viewer => 0,
        Role::Proposer => 1,
        Role::Editor => 2,
    };
    MIN_WRAP_CONTENT_LEN + write_key_count * ED25519_KEY_LEN
}

/// 包みの中身を組み、宛先とその役割に応じた鍵を作成者の署名で認証する。
fn wrap_contents(
    encoded_header: &[u8],
    creator: MemberId,
    signing: &UserSigningKey,
    recipient: &Member,
    keys: &GenerationKeys,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let role = recipient.role;
    let len = wrap_content_len(role);
    let encoded_len = u32::try_from(len).map_err(|_| Error::RecordTooLarge)?;
    let mut contents = Zeroizing::new(Vec::with_capacity(CONTENT_HEADER_LEN + len));
    contents.extend_from_slice(&WRAP_CONTENT_VERSION.to_be_bytes());
    contents.extend_from_slice(&encoded_len.to_be_bytes());
    contents.extend_from_slice(creator.as_bytes());
    let signature_start = contents.len();
    contents.extend_from_slice(&[0; SIGNATURE_LEN]);
    let role_start = contents.len();
    contents.push(role.value());
    contents.extend_from_slice(keys.topic.as_bytes());
    if role >= Role::Proposer {
        contents.extend_from_slice(keys.proposal.seed());
    }
    if role == Role::Editor {
        contents.extend_from_slice(keys.body.seed());
    }

    let signed = creator_message(
        encoded_header,
        creator,
        recipient.id,
        &contents[role_start..],
    );
    let signature = signing.sign(&signed);
    contents[signature_start..role_start].copy_from_slice(&signature);
    Ok(contents)
}

fn read_write_key(reader: &mut Reader<'_>) -> Result<WriteSigningKey, Error> {
    Ok(WriteSigningKey::from_seed(reader.array_ref()?))
}

/// 長さを前に置いたバイト列を借りる。
fn read_part<'a>(reader: &mut Reader<'a>) -> Result<&'a [u8], Error> {
    let len = usize::try_from(reader.u32()?).map_err(|_| Error::MalformedRecord)?;
    reader.take(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::member::UserPublicKeys;
    use crate::record::{Branch, RecordHeader, RecordKind, SealedRecord, SyncPageId};

    /// 「テストのメンバー構成」のA (編集)・B (編集)・C (提案)・D (閲覧)。
    struct Team {
        members: Vec<(MemberId, UserKeys)>,
        membership: Membership,
    }

    const A: usize = 0;
    const B: usize = 1;
    const C: usize = 2;
    const D: usize = 3;

    impl Team {
        fn new() -> Self {
            let roles = [Role::Editor, Role::Editor, Role::Proposer, Role::Viewer];
            let members: Vec<(MemberId, UserKeys)> = roles
                .iter()
                .map(|_| {
                    (
                        MemberId::generate().expect("メンバーのIDを作れなかった"),
                        UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった"),
                    )
                })
                .collect();
            let membership = membership(&members, &roles);
            Self {
                members,
                membership,
            }
        }

        fn id(&self, index: usize) -> MemberId {
            self.members[index].0
        }

        fn keys(&self, index: usize) -> &UserKeys {
            &self.members[index].1
        }

        fn member(&self, index: usize) -> &Member {
            self.membership
                .get(&self.id(index))
                .expect("メンバー構成にいない")
        }

        fn create(&self, generation: u64, keys: &GenerationKeys) -> GenerationRecord {
            GenerationRecord::create(
                Suite::CURRENT,
                topic(),
                generation,
                keys,
                self.id(A),
                self.keys(A),
                &self.membership,
            )
            .expect("世代の記録を作れなかった")
        }

        fn open(&self, record: &GenerationRecord, index: usize) -> Result<RoleKeys, Error> {
            record.open(self.id(index), self.keys(index), &self.membership)
        }
    }

    fn membership(members: &[(MemberId, UserKeys)], roles: &[Role]) -> Membership {
        Membership::new(
            members
                .iter()
                .zip(roles)
                .map(|((id, keys), role)| Member {
                    id: *id,
                    role: *role,
                    keys: keys.public_keys().clone(),
                })
                .collect(),
        )
        .expect("メンバー構成を作れなかった")
    }

    fn topic() -> TopicId {
        TopicId::from_bytes([7; 16])
    }

    fn header(generation: u64, keys: &GenerationKeys) -> GenerationHeader {
        GenerationHeader {
            suite: Suite::CURRENT,
            topic: topic(),
            generation,
            write_keys: keys.write_keys(),
        }
    }

    fn generation_keys() -> GenerationKeys {
        GenerationKeys::generate().expect("世代の鍵を作れなかった")
    }

    /// 受け取ったトピックの鍵が、作った世代のトピックの鍵と同じかを、レコードを開けるかで確かめる。
    fn same_topic_key(created: &GenerationKeys, received: &TopicKey) -> bool {
        let header = RecordHeader {
            kind: RecordKind::Log,
            suite: Suite::CURRENT,
            topic: topic(),
            page: SyncPageId::from_bytes([1; 16]),
            branch: Branch::Body,
            generation: 1,
        };
        SealedRecord::seal(header, b"payload", &created.topic, &created.body)
            .and_then(|record| record.verify(&created.body.verifying_key()))
            .and_then(|record| record.open(received))
            .is_ok_and(|payload| payload.as_bytes() == b"payload")
    }

    /// 宛先 `from` の包みを、宛先 `to` の包みとして付け替えた世代の記録を作る。
    fn relabel(record: &GenerationRecord, from: MemberId, to: MemberId) -> Vec<u8> {
        let mut bytes = record.as_bytes().to_vec();
        let from_at = find(&bytes, from.as_bytes());
        let to_at = find(&bytes, to.as_bytes());
        bytes[from_at..from_at + 16].copy_from_slice(to.as_bytes());
        bytes[to_at..to_at + 16].copy_from_slice(from.as_bytes());
        bytes
    }

    fn find(bytes: &[u8], needle: &[u8]) -> usize {
        bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("世代の記録の中に値が見つからなかった")
    }

    #[test]
    fn 包みを開いて役割に応じた鍵を受け取れる() {
        let team = Team::new();
        let keys = generation_keys();
        let record = GenerationRecord::parse(team.create(5, &keys).into_bytes())
            .expect("世代の記録を読めなかった");

        assert_eq!(
            *record.header(),
            header(5, &keys),
            "読んだ封筒が作った封筒と異なる"
        );
        for (index, role) in [
            (A, Role::Editor),
            (B, Role::Editor),
            (C, Role::Proposer),
            (D, Role::Viewer),
        ] {
            let received = team
                .open(&record, index)
                .expect("自分宛ての包みを開けなかった");
            assert_eq!(
                received.role(),
                role,
                "受け取った役割がメンバー構成と異なる"
            );
            assert!(
                same_topic_key(&keys, received.topic_key()),
                "受け取ったトピックの鍵が世代の鍵と異なる",
            );
            assert_eq!(
                received.proposal_key().map(WriteSigningKey::verifying_key),
                (role >= Role::Proposer).then_some(keys.write_keys().proposal),
                "{role:?}の受け取った提案の書き込みの鍵が役割と合わない",
            );
            assert_eq!(
                received.body_key().map(WriteSigningKey::verifying_key),
                (role == Role::Editor).then_some(keys.write_keys().body),
                "{role:?}の受け取った本文の書き込みの鍵が役割と合わない",
            );
        }
    }

    #[test]
    fn 役割を超えた鍵を包まない() {
        let keys = generation_keys();
        let team = Team::new();
        let generation_header = header(1, &keys).encode();
        let len = |index| {
            let recipient = team.membership.get(&team.id(index)).expect("宛先が無い");
            wrap_contents(
                &generation_header,
                team.id(A),
                team.keys(A).signing(),
                recipient,
                &keys,
            )
            .expect("包みの中身を組めなかった")
            .len()
        };

        let viewer = CONTENT_HEADER_LEN + 16 + SIGNATURE_LEN + 1 + TOPIC_KEY_LEN;
        assert_eq!(len(D), viewer, "閲覧の包みに書き込みの鍵を入れた");
        assert_eq!(
            len(C),
            viewer + ED25519_KEY_LEN,
            "提案の包みに本文の書き込みの鍵を入れた",
        );
        assert_eq!(
            len(A),
            viewer + 2 * ED25519_KEY_LEN,
            "編集の包みの鍵が足りない",
        );
    }

    #[test]
    fn ほかのメンバー宛ての包みを付け替えても開けない() {
        let team = Team::new();
        let record = team.create(1, &generation_keys());
        let relabeled = GenerationRecord::parse(relabel(&record, team.id(B), team.id(D)))
            .expect("付け替えた世代の記録を読めなかった");
        assert!(
            matches!(team.open(&relabeled, D), Err(Error::DecryptRecord)),
            "編集の役割のメンバー宛ての包みを、閲覧の役割のメンバーが開けた",
        );
    }

    #[test]
    fn 包みのinfoと追加データはそれぞれトピックと世代と宛先を含む() {
        let keys = generation_keys();
        let header = header(0x0102_0304_0506_0708, &keys);
        let recipient = MemberId::from_bytes(*b"0123456789abcdef");
        let expected_info = [
            b"nonoto/generation/wrap-info\0".as_slice(),
            &[7; 16],
            b"\x01\x02\x03\x04\x05\x06\x07\x08",
            b"0123456789abcdef",
        ]
        .concat();
        let expected_aad = [
            b"nonoto/generation/wrap-aad\0".as_slice(),
            b"NNTR\x00\x01\x03\x00\x01",
            &[7; 16],
            b"\x01\x02\x03\x04\x05\x06\x07\x08",
            &header.write_keys.proposal.to_bytes(),
            &header.write_keys.body.to_bytes(),
            b"0123456789abcdef",
        ]
        .concat();

        assert_eq!(
            header.wrap_info(&recipient),
            expected_info,
            "包みのinfoが違う"
        );
        assert_eq!(
            wrap_aad(&header.encode(), &recipient),
            expected_aad,
            "包みの追加データが違う"
        );
    }

    #[test]
    fn 別の世代に付け替えた包みを開けない() {
        let team = Team::new();
        let keys = generation_keys();
        let first = team.create(1, &keys);
        let second = team.create(2, &keys);

        // 2つ目の世代の記録の、Dの包みを、1つ目の世代の記録のDの包みに置き換える。
        let mut bytes = second.as_bytes().to_vec();
        let (_, old) = first
            .wraps
            .iter()
            .find(|(recipient, _)| *recipient == team.id(D))
            .expect("Dの包みが無い");
        let (_, new) = second
            .wraps
            .iter()
            .find(|(recipient, _)| *recipient == team.id(D))
            .expect("Dの包みが無い");
        let at = find(&bytes, &new.ciphertext);
        bytes[at..at + new.ciphertext.len()].copy_from_slice(&old.ciphertext);
        let at = find(&bytes, &new.enc);
        bytes[at..at + new.enc.len()].copy_from_slice(&old.enc);

        let spliced = GenerationRecord::parse(bytes).expect("付け替えた世代の記録を読めなかった");
        assert!(
            matches!(team.open(&spliced, D), Err(Error::DecryptRecord)),
            "別の世代の包みを開けた",
        );
    }

    #[test]
    fn 封筒と自分宛ての包みのどのバイトを書き換えても開けない() {
        let team = Team::new();
        let record = team.create(1, &generation_keys());
        let bytes = record.as_bytes().to_vec();
        // 封筒と包みの数、Dの包み (宛先・カプセル・暗号文) の範囲。ほかのメンバー宛ての包みは、Dが開くときに読まない。
        let (_, wrapped) = record
            .wraps
            .iter()
            .find(|(recipient, _)| *recipient == team.id(D))
            .expect("Dの包みが無い");
        let start = find(&bytes, team.id(D).as_bytes());
        let end = start + 16 + 4 + wrapped.enc.len() + 4 + wrapped.ciphertext.len();
        let indexes = (0..record.header_len + 2).chain(start..end);

        for index in indexes {
            let mut changed = bytes.clone();
            changed[index] ^= 0x01;
            let result = GenerationRecord::parse(changed).and_then(|record| team.open(&record, D));
            assert!(
                result.is_err(),
                "{index}バイト目を書き換えた世代の記録を開けた"
            );
        }
    }

    #[test]
    fn 編集の役割でないメンバーは世代の記録を作れない() {
        let team = Team::new();
        let keys = generation_keys();

        for index in [C, D] {
            assert!(
                matches!(
                    GenerationRecord::create(
                        Suite::CURRENT,
                        topic(),
                        1,
                        &keys,
                        team.id(index),
                        team.keys(index),
                        &team.membership,
                    ),
                    Err(Error::CreatorNotEditor)
                ),
                "編集の役割でないメンバーが世代の記録を作れた",
            );
        }
    }

    #[test]
    fn 作成者の署名鍵がメンバー構成と異なる場合は記録を作れない() {
        let team = Team::new();
        let keys = generation_keys();

        assert!(
            matches!(
                GenerationRecord::create(
                    Suite::CURRENT,
                    topic(),
                    1,
                    &keys,
                    team.id(A),
                    team.keys(B),
                    &team.membership,
                ),
                Err(Error::CreatorKeyMismatch)
            ),
            "作成者の公開鍵と合わない署名鍵で世代の記録を作れた",
        );
    }

    #[test]
    fn 作ったメンバーが受け手のメンバー構成で編集の役割でなければ拒む() {
        let team = Team::new();
        let record = team.create(1, &generation_keys());
        // 受け手の知るメンバー構成では、作ったメンバーAの役割が提案に下がっている。
        let lowered = membership(
            &team
                .members
                .iter()
                .map(|(id, keys)| (*id, keys.clone()))
                .collect::<Vec<_>>(),
            &[Role::Proposer, Role::Editor, Role::Proposer, Role::Viewer],
        );

        assert!(
            matches!(
                record.open(team.id(D), team.keys(D), &lowered),
                Err(Error::CreatorNotEditor)
            ),
            "編集の役割でないメンバーが作った世代の記録を受け付けた",
        );
    }

    #[test]
    fn 作ったメンバーの署名が合わなければ拒む() {
        let team = Team::new();
        let record = team.create(1, &generation_keys());
        // 受け手の知るAの署名の公開鍵が、作ったときの鍵と違う。
        let other = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let members: Vec<Member> = team
            .membership
            .iter()
            .map(|member| {
                let mut member = member.clone();
                if member.id == team.id(A) {
                    member.keys = UserPublicKeys {
                        wrap: member.keys.wrap.clone(),
                        signing: other.public_keys().signing,
                    };
                }
                member
            })
            .collect();
        let changed = Membership::new(members).expect("メンバー構成を作れなかった");

        assert!(
            matches!(
                record.open(team.id(D), team.keys(D), &changed),
                Err(Error::InvalidCreatorSignature)
            ),
            "作ったメンバーの署名が合わない世代の記録を受け付けた",
        );
    }

    #[test]
    fn 作成者の署名に宛先と役割と配布する鍵が結び付いている() {
        let team = Team::new();
        let keys = generation_keys();
        let header = header(1, &keys).encode();
        let recipient = team.membership.get(&team.id(B)).expect("宛先が無い");
        let signer = team.keys(A).signing();
        let payload = wrap_contents(&header, team.id(A), signer, recipient, &keys)
            .expect("包みの中身を組めなかった");
        let mut reader = Reader::new(&payload);
        assert_eq!(
            reader.u16().expect("中身の版が無い"),
            WRAP_CONTENT_VERSION,
            "包みの中身の版が違う"
        );
        let len = reader.u32().expect("中身の長さが無い");
        assert_eq!(
            usize::try_from(len).expect("中身が大きすぎる"),
            reader.remaining().len(),
            "中身の長さが違う"
        );
        let creator = MemberId::from_bytes(reader.array().expect("作成者のIDが無い"));
        let signature = reader.array_ref().expect("作成者の署名が無い");
        let role_and_keys = reader.remaining();
        let check = |recipient, content: &[u8]| {
            let message = creator_message(&header, creator, recipient, content);
            signer.verifying_key().verify_creator(&message, signature)
        };

        check(recipient.id, role_and_keys).expect("元の配布内容の署名が合わない");
        assert!(
            matches!(
                check(team.id(C), role_and_keys),
                Err(Error::InvalidCreatorSignature)
            ),
            "別の宛先の署名を受け付けた"
        );
        let mut changed = Zeroizing::new(role_and_keys.to_vec());
        for (at, field) in [
            (0, "役割"),
            (1, "トピックの鍵"),
            (1 + TOPIC_KEY_LEN, "提案の書き込み鍵"),
            (1 + TOPIC_KEY_LEN + ED25519_KEY_LEN, "本文の書き込み鍵"),
        ] {
            changed[at] ^= 1;
            assert!(
                matches!(
                    check(recipient.id, &changed),
                    Err(Error::InvalidCreatorSignature)
                ),
                "{field}が違う署名を受け付けた"
            );
            changed[at] ^= 1;
        }
    }

    #[test]
    fn 受け手の役割と包みの役割が違えば拒む() {
        let team = Team::new();
        let record = team.create(1, &generation_keys());
        // 受け手の知るメンバー構成では、Cの役割が閲覧に下がっている。
        let lowered = membership(
            &team
                .members
                .iter()
                .map(|(id, keys)| (*id, keys.clone()))
                .collect::<Vec<_>>(),
            &[Role::Editor, Role::Editor, Role::Viewer, Role::Viewer],
        );

        assert!(
            matches!(
                record.open(team.id(C), team.keys(C), &lowered),
                Err(Error::WrappedKeysMismatch)
            ),
            "役割より多い鍵の包みを受け付けた",
        );
    }

    #[test]
    fn 宛先にいないメンバーは開けない() {
        let team = Team::new();
        let record = team.create(1, &generation_keys());
        let outsider = MemberId::generate().expect("メンバーのIDを作れなかった");
        let outsider_keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let mut members: Vec<Member> = team.membership.iter().cloned().collect();
        members.push(Member {
            id: outsider,
            role: Role::Viewer,
            keys: outsider_keys.public_keys().clone(),
        });
        let joined = Membership::new(members).expect("メンバー構成を作れなかった");

        assert!(
            matches!(
                record.open(outsider, &outsider_keys, &joined),
                Err(Error::NotRecipient)
            ),
            "宛先にいないメンバーが開けた",
        );
    }

    fn encode_wraps_for_test(header: &GenerationHeader, wraps: &[(MemberId, Wrapped)]) -> Vec<u8> {
        let mut bytes = header.encode();
        let count = u16::try_from(wraps.len()).expect("テストの包みが多すぎる");
        bytes.extend_from_slice(&count.to_be_bytes());
        for (recipient, wrapped) in wraps {
            bytes.extend_from_slice(recipient.as_bytes());
            for part in [&wrapped.enc, &wrapped.ciphertext] {
                let len = u32::try_from(part.len()).expect("テストの包みが大きすぎる");
                bytes.extend_from_slice(&len.to_be_bytes());
                bytes.extend_from_slice(part);
            }
        }
        bytes
    }

    /// 作ったメンバーAが、宛先 `index` に世代の鍵 `keys` を包む中身を組む。
    fn contents_for(
        team: &Team,
        record: &GenerationRecord,
        index: usize,
        keys: &GenerationKeys,
    ) -> Zeroizing<Vec<u8>> {
        wrap_contents(
            &record.header().encode(),
            team.id(A),
            team.keys(A).signing(),
            team.member(index),
            keys,
        )
        .expect("包みの中身を組めなかった")
    }

    /// 宛先 `index` の包みを、渡した中身で包み直した世代の記録を作る。
    fn rewrap(
        team: &Team,
        record: &GenerationRecord,
        index: usize,
        contents: &[u8],
    ) -> GenerationRecord {
        let header = record.header();
        let encoded = header.encode();
        let recipient = team.member(index);
        let wrapped = header
            .suite
            .wrap(
                &recipient.keys.wrap,
                &header.wrap_info(&recipient.id),
                &wrap_aad(&encoded, &recipient.id),
                contents,
            )
            .expect("包みを作れなかった");
        let mut wraps = record.wraps.clone();
        let position = wraps
            .iter()
            .position(|(id, _)| *id == recipient.id)
            .expect("宛先が無い");
        wraps[position].1 = wrapped;
        GenerationRecord::parse(encode_wraps_for_test(header, &wraps))
            .expect("包み直した世代の記録を読めなかった")
    }

    #[test]
    fn 知らない版の世代の包みの中身は鍵を返さずに拒む() {
        let team = Team::new();
        let keys = generation_keys();
        let record = team.create(1, &keys);
        let mut contents = contents_for(&team, &record, D, &keys);
        contents[..2].copy_from_slice(&2u16.to_be_bytes());
        let record = rewrap(&team, &record, D, &contents);

        assert!(
            matches!(
                team.open(&record, D),
                Err(Error::UnsupportedContentVersion(2))
            ),
            "知らない版の世代の包みの中身を受け付けた"
        );
    }

    #[test]
    fn 包みの中の書き込みの鍵が封筒の公開鍵と対にならなければ拒む() {
        let team = Team::new();
        let record = team.create(1, &generation_keys());
        // 作ったメンバーAが、封筒の公開鍵とは別の世代の鍵を包んで署名した包み。
        let other = generation_keys();

        for index in [A, C] {
            let contents = contents_for(&team, &record, index, &other);
            let record = rewrap(&team, &record, index, &contents);
            assert!(
                matches!(team.open(&record, index), Err(Error::WrappedKeysMismatch)),
                "封筒の公開鍵と対にならない書き込みの鍵を受け取った",
            );
        }
    }

    #[test]
    fn 中身の長さが包みの中の役割と合わなければ拒む() {
        let team = Team::new();
        let keys = generation_keys();
        let record = team.create(1, &keys);
        // 閲覧の役割の包みに書き込みの鍵の種の長さを足し、中身の長さも合わせる。
        let mut contents = contents_for(&team, &record, D, &keys);
        contents.extend_from_slice(&[0; ED25519_KEY_LEN]);
        let len = u32::try_from(contents.len() - CONTENT_HEADER_LEN).expect("中身が大きすぎる");
        contents[2..CONTENT_HEADER_LEN].copy_from_slice(&len.to_be_bytes());
        let record = rewrap(&team, &record, D, &contents);

        assert!(
            matches!(team.open(&record, D), Err(Error::MalformedRecord)),
            "役割に合わない長さの包みの中身を受け付けた",
        );
    }

    #[test]
    fn 詰め物を足した包みを開ける() {
        let team = Team::new();
        let keys = generation_keys();
        let record = team.create(1, &keys);
        let mut contents = contents_for(&team, &record, C, &keys);
        contents.extend_from_slice(&[0xff; 64]);
        let record = rewrap(&team, &record, C, &contents);

        let received = team
            .open(&record, C)
            .expect("詰め物を足した包みを開けなかった");
        assert!(
            same_topic_key(&keys, received.topic_key()),
            "詰め物を足した包みのトピックの鍵が世代の鍵と異なる",
        );
        assert_eq!(
            received.proposal_key().map(WriteSigningKey::verifying_key),
            Some(keys.write_keys().proposal),
            "詰め物を足した包みの提案の書き込みの鍵が世代の鍵と異なる",
        );
    }

    #[test]
    fn ログのレコードと世代の記録を取り違えて読まない() {
        let team = Team::new();
        let keys = generation_keys();
        let generation = team.create(1, &keys).into_bytes();
        let header = RecordHeader {
            kind: RecordKind::Log,
            suite: Suite::CURRENT,
            topic: topic(),
            page: SyncPageId::from_bytes([1; 16]),
            branch: Branch::Body,
            generation: 1,
        };
        let log = SealedRecord::seal(header, b"payload", &keys.topic, &keys.body)
            .expect("ログのレコードを封じられなかった")
            .into_bytes();

        assert!(
            matches!(GenerationRecord::parse(log), Err(Error::MalformedRecord)),
            "ログのレコードを世代の記録として読んだ",
        );
        assert!(
            matches!(SealedRecord::parse(generation), Err(Error::MalformedRecord)),
            "世代の記録をログのレコードとして読んだ",
        );
    }

    #[test]
    fn 世代の包みの数と宛先の並びを検査する() {
        let record = Team::new().create(1, &generation_keys());
        let mut incorrect_count = record.as_bytes().to_vec();
        incorrect_count[record.header_len..record.header_len + 2]
            .copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(
            matches!(
                GenerationRecord::parse(incorrect_count),
                Err(Error::MalformedRecord)
            ),
            "包みの実体に合わない件数を受け付けた"
        );
        let mut wraps = record.wraps.clone();
        wraps[1].0 = wraps[0].0;
        let duplicate = encode_wraps_for_test(record.header(), &wraps);
        assert!(
            matches!(
                GenerationRecord::parse(duplicate),
                Err(Error::MalformedRecord)
            ),
            "重複した宛先を受け付けた"
        );
    }

    #[test]
    fn hpkeの構造に合わない長さの包みを読み取りで拒む() {
        let record = Team::new().create(1, &generation_keys());
        let mut wraps = record.wraps.clone();
        wraps[0].1.enc.push(0);
        let bytes = encode_wraps_for_test(record.header(), &wraps);
        assert!(
            matches!(GenerationRecord::parse(bytes), Err(Error::MalformedRecord)),
            "方式で決めた長さと異なるカプセルを受け付けた"
        );
        let mut wraps = record.wraps.clone();
        let (_, tag_len) = record.header().suite.wrap_lengths();
        wraps[0].1.ciphertext.truncate(tag_len - 1);
        let bytes = encode_wraps_for_test(record.header(), &wraps);
        assert!(
            matches!(GenerationRecord::parse(bytes), Err(Error::MalformedRecord)),
            "認証タグより短い暗号文を受け付けた"
        );
    }

    #[test]
    fn 途中で切れた世代の記録を拒む() {
        let team = Team::new();
        let bytes = team.create(1, &generation_keys()).into_bytes();

        for len in 0..bytes.len() {
            assert!(
                GenerationRecord::parse(bytes[..len].to_vec()).is_err(),
                "{len}バイトで切れた世代の記録を受け付けた",
            );
        }
    }

    #[test]
    fn 世代の鍵のデバッグ表示に秘密鍵を出さない() {
        assert_eq!(
            format!("{:?}", generation_keys()),
            "GenerationKeys(..)",
            "世代の鍵を表示した",
        );
    }
}
