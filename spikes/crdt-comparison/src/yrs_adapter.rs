use std::sync::{Arc, Mutex};

use yrs::types::Delta;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{
    ClientID, Doc, GetString, Observable, OffsetKind, Options, ReadTxn, Snapshot, StateVector,
    Text, TextRef, Transact, Update,
};

use crate::{DeltaOp, Replica, ReplicaError, TextChange, changes_from_delta};

const NAME: &str = "yrs";

/// 本文を入れるルートの型の名前。ルートの型は名前で決まるため、どの複製でも同じ型を指す。
const TEXT_NAME: &str = "body";

/// 届いた更新の差分を受け取る購読の名前。
const OBSERVER_KEY: &str = "crdt-comparison";

/// yrsの文書の複製。
///
/// 位置は `OffsetKind::Bytes` (UTF-8のバイト数) で数える。
/// 版の本文を取り出せるよう、削除した文字を消さない設定 (`skip_gc`) で作る。
pub struct YrsReplica {
    doc: Doc,
    text: TextRef,
}

fn options(client_id: Option<ClientID>, skip_gc: bool) -> Options {
    let defaults = Options::default();
    Options {
        // 既定のクライアントIDは乱数で決まるため、複製ごとに別の書き手になる。
        client_id: client_id.unwrap_or(defaults.client_id),
        offset_kind: OffsetKind::Bytes,
        skip_gc,
        ..defaults
    }
}

impl YrsReplica {
    fn with_doc(doc: Doc) -> Self {
        let text = doc.get_or_insert_text(TEXT_NAME);
        Self { doc, text }
    }

    fn load(self, snapshot: &[u8]) -> Result<Self, ReplicaError> {
        let update = Update::decode_v1(snapshot).map_err(|_| ReplicaError::Load(NAME))?;
        self.doc
            .transact_mut()
            .apply_update(update)
            .map_err(|_| ReplicaError::Load(NAME))?;
        Ok(self)
    }

    /// 手元の編集を1つのトランザクションで当て、その間に増えた分を更新として取り出す。
    fn edit(&mut self, f: impl FnOnce(&TextRef, &mut yrs::TransactionMut)) -> Vec<u8> {
        let mut txn = self.doc.transact_mut();
        f(&self.text, &mut txn);
        txn.encode_update_v1()
    }
}

impl Replica for YrsReplica {
    const NAME: &'static str = NAME;

    fn new() -> Self {
        Self::with_doc(Doc::with_options(options(None, true)))
    }

    fn from_snapshot(snapshot: &[u8]) -> Result<Self, ReplicaError> {
        Self::new().load(snapshot)
    }

    fn with_writer_id(writer_id: u32) -> Self {
        let client_id = ClientID::new(u64::from(writer_id));
        Self::with_doc(Doc::with_options(options(Some(client_id), true)))
    }

    fn from_snapshot_with_writer_id(snapshot: &[u8], writer_id: u32) -> Result<Self, ReplicaError> {
        Self::with_writer_id(writer_id).load(snapshot)
    }

    fn text(&self) -> String {
        self.text.get_string(&self.doc.transact())
    }

    fn insert(&mut self, pos: usize, text: &str) -> Result<Vec<u8>, ReplicaError> {
        let pos = u32::try_from(pos).map_err(|_| ReplicaError::Edit(NAME))?;
        Ok(self.edit(|body, txn| body.insert(txn, pos, text)))
    }

    fn delete(&mut self, pos: usize, len: usize) -> Result<Vec<u8>, ReplicaError> {
        let pos = u32::try_from(pos).map_err(|_| ReplicaError::Edit(NAME))?;
        let len = u32::try_from(len).map_err(|_| ReplicaError::Edit(NAME))?;
        Ok(self.edit(|body, txn| body.remove_range(txn, pos, len)))
    }

    fn apply_update(&mut self, update: &[u8]) -> Result<(), ReplicaError> {
        let update = Update::decode_v1(update).map_err(|_| ReplicaError::Apply(NAME))?;
        // 前の更新が届いていない更新は、文書の中に保留され、前の更新が届いたときに当たる。
        self.doc
            .transact_mut()
            .apply_update(update)
            .map_err(|_| ReplicaError::Apply(NAME))
    }

    fn apply_update_with_changes(
        &mut self,
        update: &[u8],
    ) -> Result<Vec<TextChange>, ReplicaError> {
        // 差分の長さは、文書の `offset_kind` (UTF-8のバイト数) で数えられる。
        let ops = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&ops);
        self.text.observe(OBSERVER_KEY, move |txn, event| {
            let mut sink = sink.lock().expect("差分の置き場を開けなかった");
            for delta in event.delta(txn) {
                sink.push(match delta {
                    Delta::Retain(len, _) => DeltaOp::Retain(*len as usize),
                    Delta::Deleted(len) => DeltaOp::Delete(*len as usize),
                    Delta::Inserted(value, _) => DeltaOp::Insert(value.clone().to_string(txn)),
                });
            }
        });
        let result = self.apply_update(update);
        self.text.unobserve(OBSERVER_KEY);
        result?;
        let ops = std::mem::take(&mut *ops.lock().expect("差分の置き場を開けなかった"));
        Ok(changes_from_delta(ops))
    }

    fn snapshot(&mut self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    fn compacted_snapshot(&mut self) -> Option<Vec<u8>> {
        // 削除した文字を消す設定の文書に読み込み直し、書き出す。
        let compacted = Self::with_doc(Doc::with_options(options(None, false)))
            .load(&self.snapshot())
            .ok()?;
        Some(
            compacted
                .doc
                .transact()
                .encode_state_as_update_v1(&StateVector::default()),
        )
    }

    fn version(&mut self) -> Vec<u8> {
        self.doc.transact().snapshot().encode_v1()
    }

    fn text_at(&mut self, version: &[u8]) -> Result<String, ReplicaError> {
        let snapshot = Snapshot::decode_v1(version).map_err(|_| ReplicaError::Version(NAME))?;
        let mut encoder = EncoderV1::new();
        self.doc
            .transact()
            .encode_state_from_snapshot(&snapshot, &mut encoder)
            .map_err(|_| ReplicaError::Version(NAME))?;
        let past = Self::new()
            .load(&encoder.to_vec())
            .map_err(|_| ReplicaError::Version(NAME))?;
        Ok(past.text())
    }

    fn fork(&mut self) -> Result<Self, ReplicaError> {
        // 分岐の専用の入口は無いため、スナップショットから別の書き手の文書を作る。
        self.replicate()
    }

    fn merge(&mut self, branch: &mut Self) -> Result<(), ReplicaError> {
        let state = self.doc.transact().state_vector();
        let update = branch.doc.transact().encode_state_as_update_v1(&state);
        self.apply_update(&update)
            .map_err(|_| ReplicaError::Merge(NAME))
    }
}
