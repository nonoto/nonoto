use automerge::{
    ActorId, AutoCommit, ChangeHash, LoadOptions, ObjId, ObjType, PatchAction, ROOT, ReadDoc,
    TextEncoding, transaction::Transactable,
};

use crate::{Replica, ReplicaError, TextChange};

const NAME: &str = "automerge";

/// 本文を入れるテキストのオブジェクトを置く、ルートのキー。
const TEXT_KEY: &str = "body";

/// 位置を数える単位。UTF-8のバイト数で数える。
const ENCODING: TextEncoding = TextEncoding::Utf8CodeUnit;

/// 変更のハッシュのバイト数。版は、先端の変更のハッシュを並べたバイト列で表す。
const HASH_LEN: usize = 32;

/// Automergeの文書の複製。
///
/// 位置は `TextEncoding::Utf8CodeUnit` (UTF-8のバイト数) で数える。
pub struct AutomergeReplica {
    doc: AutoCommit,
    text: ObjId,
}

impl AutomergeReplica {
    fn with_doc(mut doc: AutoCommit) -> Self {
        // テキストのオブジェクトのIDに書き手が入るため、書き手は作成前に設定する。
        let text = doc
            .put_object(ROOT, TEXT_KEY, ObjType::Text)
            .expect("空の文書のルートにテキストを置けなかった");
        doc.commit();
        Self { doc, text }
    }

    /// 手元の編集を1つの変更として確定し、その変更を更新として取り出す。
    fn edit(
        &mut self,
        f: impl FnOnce(&mut AutoCommit, &ObjId) -> Result<(), automerge::AutomergeError>,
    ) -> Result<Vec<u8>, ReplicaError> {
        let heads = self.doc.get_heads();
        f(&mut self.doc, &self.text).map_err(|_| ReplicaError::Edit(NAME))?;
        self.doc.commit();
        Ok(self.doc.save_after(&heads))
    }
}

fn decode_heads(version: &[u8]) -> Result<Vec<ChangeHash>, ReplicaError> {
    if !version.len().is_multiple_of(HASH_LEN) {
        return Err(ReplicaError::Version(NAME));
    }
    version
        .chunks(HASH_LEN)
        .map(|chunk| ChangeHash::try_from(chunk).map_err(|_| ReplicaError::Version(NAME)))
        .collect()
}

impl Replica for AutomergeReplica {
    const NAME: &'static str = NAME;

    fn new() -> Self {
        Self::with_doc(AutoCommit::new_with_encoding(ENCODING))
    }

    fn with_writer_id(writer_id: u32) -> Self {
        Self::with_doc(
            AutoCommit::new_with_encoding(ENCODING)
                .with_actor(ActorId::from(writer_id.to_be_bytes().to_vec())),
        )
    }

    fn from_snapshot(snapshot: &[u8]) -> Result<Self, ReplicaError> {
        // 読み込んだ文書の書き手は乱数で決まるため、元の文書とは別の書き手になる。
        let doc =
            AutoCommit::load_with_options(snapshot, LoadOptions::new().text_encoding(ENCODING))
                .map_err(|_| ReplicaError::Load(NAME))?;
        let text = match doc.get(ROOT, TEXT_KEY) {
            Ok(Some((_, id))) => id,
            _ => return Err(ReplicaError::Load(NAME)),
        };
        Ok(Self { doc, text })
    }

    fn from_snapshot_with_writer_id(snapshot: &[u8], writer_id: u32) -> Result<Self, ReplicaError> {
        let mut replica = Self::from_snapshot(snapshot)?;
        replica
            .doc
            .set_actor(ActorId::from(writer_id.to_be_bytes().to_vec()));
        Ok(replica)
    }

    fn text(&self) -> String {
        self.doc
            .text(&self.text)
            .expect("本文のテキストのオブジェクトを読めなかった")
    }

    fn insert(&mut self, pos: usize, text: &str) -> Result<Vec<u8>, ReplicaError> {
        self.edit(|doc, obj| doc.splice_text(obj, pos, 0, text))
    }

    fn delete(&mut self, pos: usize, len: usize) -> Result<Vec<u8>, ReplicaError> {
        let len = isize::try_from(len).map_err(|_| ReplicaError::Edit(NAME))?;
        self.edit(|doc, obj| doc.splice_text(obj, pos, len, ""))
    }

    fn apply_update(&mut self, update: &[u8]) -> Result<(), ReplicaError> {
        // 依存する変更が届いていない変更は、文書の中に保留され、依存が届いたときに当たる。
        self.doc
            .load_incremental(update)
            .map(|_| ())
            .map_err(|_| ReplicaError::Apply(NAME))
    }

    fn apply_update_with_changes(
        &mut self,
        update: &[u8],
    ) -> Result<Vec<TextChange>, ReplicaError> {
        // 差分の起点を今の版に置き、読み込んだ変更だけを差分の索引に記録させる。
        self.doc.update_diff_cursor();
        let result = self.apply_update(update);
        let patches = self.doc.diff_incremental();
        // 手元の編集で索引を作り続けないよう、記録を止める。
        self.doc.reset_diff_cursor();
        result?;
        // パッチの位置は、文書の単位 (UTF-8のバイト数) で、前のパッチを当てたあとの本文で数えられる。
        Ok(patches
            .into_iter()
            .filter(|patch| patch.obj == self.text)
            .filter_map(|patch| match patch.action {
                PatchAction::SpliceText { index, value, .. } => Some(TextChange {
                    start: index,
                    end: index,
                    insert: value.make_string(),
                }),
                PatchAction::DeleteSeq { index, length } => Some(TextChange {
                    start: index,
                    end: index + length,
                    insert: String::new(),
                }),
                _ => None,
            })
            .collect())
    }

    fn snapshot(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    fn compacted_snapshot(&mut self) -> Option<Vec<u8>> {
        // 履歴をまとめる公開の入口は無い。
        None
    }

    fn version(&mut self) -> Vec<u8> {
        self.doc.get_heads().iter().flat_map(|h| h.0).collect()
    }

    fn text_at(&mut self, version: &[u8]) -> Result<String, ReplicaError> {
        let heads = decode_heads(version)?;
        self.doc
            .text_at(&self.text, &heads)
            .map_err(|_| ReplicaError::Version(NAME))
    }

    fn fork(&mut self) -> Result<Self, ReplicaError> {
        // 分岐の書き手は乱数で決まり、元の文書とは別の書き手になる。
        Ok(Self {
            doc: self.doc.fork(),
            text: self.text.clone(),
        })
    }

    fn merge(&mut self, branch: &mut Self) -> Result<(), ReplicaError> {
        self.doc
            .merge(&mut branch.doc)
            .map(|_| ())
            .map_err(|_| ReplicaError::Merge(NAME))
    }
}
