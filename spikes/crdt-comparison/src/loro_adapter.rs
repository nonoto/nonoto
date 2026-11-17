use std::sync::{Arc, Mutex};

use loro::event::{Diff, DiffEvent};
use loro::{ContainerTrait, ExportMode, Frontiers, LoroDoc, LoroText, TextDelta};

use crate::{DeltaOp, Replica, ReplicaError, TextChange, changes_from_delta};

const NAME: &str = "loro";

/// 本文を入れるルートのコンテナの名前。ルートのコンテナは名前で決まるため、どの複製でも同じコンテナを指す。
const TEXT_NAME: &str = "body";

/// Loroの文書の複製。
///
/// 位置は `insert_utf8`・`delete_utf8` で、UTF-8のバイト数で数える。
/// 届いた更新の差分はUnicodeのコードポイントで数えられ、削除した長さをUTF-8のバイト数にするには
/// 変更の前の本文が要るため、本文の写し (`body`) を持つ。
pub struct LoroReplica {
    doc: LoroDoc,
    text: LoroText,
    body: String,
}

/// 届いた差分の、コードポイントで数えた操作。
enum CharOp {
    Retain(usize),
    Delete(usize),
    Insert(String),
}

/// `text` の `from` バイト目から `chars` 文字分のバイト数を返す。
fn byte_len_of_chars(text: &str, from: usize, chars: usize) -> usize {
    text[from..]
        .char_indices()
        .nth(chars)
        .map_or(text.len() - from, |(i, _)| i)
}

impl LoroReplica {
    fn with_doc(doc: LoroDoc) -> Self {
        let text = doc.get_text(TEXT_NAME);
        let body = text.to_string();
        Self { doc, text, body }
    }

    /// 手元の編集を確定し、編集の前の版からの差分を更新として取り出す。
    fn edit(
        &mut self,
        f: impl FnOnce(&LoroText) -> loro::LoroResult<()>,
    ) -> Result<Vec<u8>, ReplicaError> {
        let before = self.doc.oplog_vv();
        f(&self.text).map_err(|_| ReplicaError::Edit(NAME))?;
        self.doc.commit();
        self.doc
            .export(ExportMode::updates(&before))
            .map_err(|_| ReplicaError::Edit(NAME))
    }

    /// 更新を読み込み、本文の差分をUTF-8のバイト数の変更の列にして、本文の写しにも当てる。
    fn import_with_changes(&mut self, update: &[u8]) -> Result<Vec<TextChange>, ReplicaError> {
        let ops = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&ops);
        let subscription = self.doc.subscribe(
            &self.text.id(),
            Arc::new(move |event: DiffEvent| {
                let mut sink = sink.lock().expect("差分の置き場を開けなかった");
                for container in event.events {
                    if let Diff::Text(deltas) = container.diff {
                        for delta in deltas {
                            sink.push(match delta {
                                TextDelta::Retain { retain, .. } => CharOp::Retain(retain),
                                TextDelta::Delete { delete } => CharOp::Delete(delete),
                                TextDelta::Insert { insert, .. } => CharOp::Insert(insert),
                            });
                        }
                    }
                }
            }),
        );
        let result = self.doc.import(update);
        drop(subscription);
        result.map_err(|_| ReplicaError::Apply(NAME))?;

        let ops = std::mem::take(&mut *ops.lock().expect("差分の置き場を開けなかった"));
        // 保持と削除は変更の前の本文で数えるため、写しの上で読み進めてバイト数にする。
        let mut old = 0;
        let byte_ops: Vec<DeltaOp> = ops
            .into_iter()
            .map(|op| match op {
                CharOp::Retain(chars) => {
                    let len = byte_len_of_chars(&self.body, old, chars);
                    old += len;
                    DeltaOp::Retain(len)
                }
                CharOp::Delete(chars) => {
                    let len = byte_len_of_chars(&self.body, old, chars);
                    old += len;
                    DeltaOp::Delete(len)
                }
                CharOp::Insert(text) => DeltaOp::Insert(text),
            })
            .collect();
        let changes = changes_from_delta(byte_ops);
        for change in &changes {
            self.body
                .replace_range(change.start..change.end, &change.insert);
        }
        Ok(changes)
    }
}

impl Replica for LoroReplica {
    const NAME: &'static str = NAME;

    fn new() -> Self {
        // 書き手 (ピアID) は乱数で決まるため、複製ごとに別の書き手になる。
        Self::with_doc(LoroDoc::new())
    }

    fn with_writer_id(writer_id: u32) -> Self {
        let doc = LoroDoc::new();
        doc.set_peer_id(u64::from(writer_id))
            .expect("空の文書の書き手IDを設定できなかった");
        Self::with_doc(doc)
    }

    fn from_snapshot(snapshot: &[u8]) -> Result<Self, ReplicaError> {
        let doc = LoroDoc::from_snapshot(snapshot).map_err(|_| ReplicaError::Load(NAME))?;
        Ok(Self::with_doc(doc))
    }

    fn from_snapshot_with_writer_id(snapshot: &[u8], writer_id: u32) -> Result<Self, ReplicaError> {
        let replica = Self::from_snapshot(snapshot)?;
        replica
            .doc
            .set_peer_id(u64::from(writer_id))
            .map_err(|_| ReplicaError::Load(NAME))?;
        Ok(replica)
    }

    fn text(&self) -> String {
        self.text.to_string()
    }

    fn insert(&mut self, pos: usize, text: &str) -> Result<Vec<u8>, ReplicaError> {
        let update = self.edit(|body| body.insert_utf8(pos, text))?;
        self.body.insert_str(pos, text);
        Ok(update)
    }

    fn delete(&mut self, pos: usize, len: usize) -> Result<Vec<u8>, ReplicaError> {
        let update = self.edit(|body| body.delete_utf8(pos, len))?;
        self.body.replace_range(pos..pos + len, "");
        Ok(update)
    }

    fn apply_update(&mut self, update: &[u8]) -> Result<(), ReplicaError> {
        // 前の更新が届いていない更新は、文書の中に保留され、前の更新が届いたときに当たる。
        self.import_with_changes(update).map(|_| ())
    }

    fn apply_update_with_changes(
        &mut self,
        update: &[u8],
    ) -> Result<Vec<TextChange>, ReplicaError> {
        self.import_with_changes(update)
    }

    fn snapshot(&mut self) -> Vec<u8> {
        self.doc
            .export(ExportMode::Snapshot)
            .expect("スナップショットを書き出せなかった")
    }

    fn compacted_snapshot(&mut self) -> Option<Vec<u8>> {
        // 今の時点より前の履歴を持たないスナップショットにする。
        self.doc
            .export(ExportMode::shallow_snapshot(&self.doc.oplog_frontiers()))
            .ok()
    }

    fn version(&mut self) -> Vec<u8> {
        self.doc.oplog_frontiers().encode()
    }

    fn text_at(&mut self, version: &[u8]) -> Result<String, ReplicaError> {
        let frontiers = Frontiers::decode(version).map_err(|_| ReplicaError::Version(NAME))?;
        let past = self
            .doc
            .fork_at(&frontiers)
            .map_err(|_| ReplicaError::Version(NAME))?;
        Ok(past.get_text(TEXT_NAME).to_string())
    }

    fn fork(&mut self) -> Result<Self, ReplicaError> {
        // 分岐は、別のピアIDを持つ別の文書になる。
        Ok(Self::with_doc(self.doc.fork()))
    }

    fn merge(&mut self, branch: &mut Self) -> Result<(), ReplicaError> {
        let update = branch
            .doc
            .export(ExportMode::updates(&self.doc.oplog_vv()))
            .map_err(|_| ReplicaError::Merge(NAME))?;
        self.import_with_changes(&update)
            .map(|_| ())
            .map_err(|_| ReplicaError::Merge(NAME))
    }
}
