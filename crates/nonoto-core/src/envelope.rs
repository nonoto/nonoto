//! 受け取ったバイト列を、封筒の種類で振り分ける。
//!
//! 中継は、届いたレコードがページのレコード (ログ・スナップショット) か世代の記録かを、封筒の先頭だけで見分ける。

use crate::error::Error;
use crate::generation::GenerationRecord;
use crate::record::{KIND_GENERATION, Reader, SealedRecord, decode_prefix};

/// 封筒を読んだレコード。書き込みの署名と、世代の記録の包みはまだ確かめていない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Envelope {
    /// ページのログのレコードかスナップショット。
    Page(SealedRecord),
    /// 世代の記録。封筒の値と包みの並びを持ち大きいため、箱に入れる。
    Generation(Box<GenerationRecord>),
}

impl Envelope {
    /// 封筒の先頭 (識別の印・封筒の版・種類・方式のID) を読み、種類に応じて封筒の全体を読む。
    ///
    /// 知らない封筒の版・方式のIDなら、それぞれ [`Error::UnsupportedEnvelopeVersion`]・
    /// [`Error::UnsupportedSuite`] を返す。
    pub fn parse(bytes: Vec<u8>) -> Result<Self, Error> {
        let (kind, _) = decode_prefix(&mut Reader::new(&bytes))?;
        if kind == KIND_GENERATION {
            GenerationRecord::parse(bytes).map(|record| Self::Generation(Box::new(record)))
        } else {
            SealedRecord::parse(bytes).map(Self::Page)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{Suite, TopicKey, WriteSigningKey};
    use crate::generation::GenerationKeys;
    use crate::member::{Member, MemberId, Membership, Role, UserKeys};
    use crate::record::{Branch, RecordHeader, RecordKind, SyncPageId, TopicId};

    #[test]
    fn ページのレコードと世代の記録を封筒の種類で振り分ける() {
        let topic = TopicId::from_bytes([1; 16]);
        let record = SealedRecord::seal(
            RecordHeader {
                kind: RecordKind::Log,
                suite: Suite::CURRENT,
                topic,
                page: SyncPageId::from_bytes([2; 16]),
                branch: Branch::Body,
                generation: 1,
            },
            b"payload",
            &TopicKey::generate().expect("トピックの鍵を作れなかった"),
            &WriteSigningKey::generate().expect("書き込みの鍵を作れなかった"),
        )
        .expect("レコードを封じられなかった");
        assert!(
            matches!(Envelope::parse(record.into_bytes()), Ok(Envelope::Page(_))),
            "ページのレコードを、ページのレコードとして読めなかった",
        );

        let id = MemberId::generate().expect("メンバーのIDを作れなかった");
        let keys = UserKeys::generate(Suite::CURRENT).expect("利用者の鍵を作れなかった");
        let membership = Membership::new(vec![Member {
            id,
            role: Role::Editor,
            keys: keys.public_keys().clone(),
        }])
        .expect("メンバー構成を作れなかった");
        let generation_keys = GenerationKeys::generate().expect("世代の鍵を作れなかった");
        let generation = GenerationRecord::create(
            Suite::CURRENT,
            topic,
            1,
            &generation_keys,
            id,
            &keys,
            &membership,
        )
        .expect("世代の記録を作れなかった");
        assert!(
            matches!(
                Envelope::parse(generation.into_bytes()),
                Ok(Envelope::Generation(_))
            ),
            "世代の記録を、世代の記録として読めなかった",
        );
    }

    #[test]
    fn 知らない封筒の版のバイト列を振り分けずに止める() {
        let mut bytes = b"NNTR".to_vec();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 64]);

        assert!(
            matches!(
                Envelope::parse(bytes),
                Err(Error::UnsupportedEnvelopeVersion(2))
            ),
            "知らない封筒の版のバイト列を振り分けた",
        );
    }
}
