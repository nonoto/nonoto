//! ページの本文を持つCRDTのライブラリを、同じ操作で比べるための実装。
//!
//! 候補ごとのアダプターが [`Replica`] を実装する。
//! テストと計測は [`Replica`] だけを使って書き、すべての候補に同じ操作の列を当てる。

#![forbid(unsafe_code)]

mod automerge_adapter;
pub mod data;
mod loro_adapter;
pub mod measure;
pub mod rng;
mod yrs_adapter;

pub use crate::automerge_adapter::AutomergeReplica;
pub use crate::loro_adapter::LoroReplica;
pub use crate::yrs_adapter::YrsReplica;

/// アダプターの操作の失敗。
///
/// ライブラリのエラーの中身には本文が含まれうるため、種類だけを持ち、メッセージに本文を出さない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaError {
    #[error("{0}が編集を受け付けなかった")]
    Edit(&'static str),
    #[error("{0}が更新を当てられなかった")]
    Apply(&'static str),
    #[error("{0}がスナップショットを読み込めなかった")]
    Load(&'static str),
    #[error("{0}が版を扱えなかった")]
    Version(&'static str),
    #[error("{0}が分岐をマージできなかった")]
    Merge(&'static str),
}

/// 本文に起きた変更の1件。`start..end` (UTF-8のバイト位置) を `insert` に置き換える。
///
/// 変更の列は先頭から順に当て、各変更の位置は、その前の変更を当てたあとの本文で数える。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChange {
    pub start: usize,
    pub end: usize,
    pub insert: String,
}

impl TextChange {
    /// 変更の列を、変更の前の本文に当てる。
    pub fn apply_all(text: &str, changes: &[TextChange]) -> String {
        let mut text = text.to_owned();
        for change in changes {
            text.replace_range(change.start..change.end, &change.insert);
        }
        text
    }
}

/// ページ1件の本文を持つ、文書の1つの複製。
///
/// 位置と長さは、本文のUTF-8のバイト数で数える。
/// 位置の単位の変換は、各アダプターの中に閉じる。
pub trait Replica: Sized {
    /// 結果の出力に使う候補の名前。
    const NAME: &'static str;

    /// 空の文書を作る。
    ///
    /// 別々に作った文書どうしは、同じ文書の複製にならない。
    /// 複製は [`Replica::replicate`] か [`Replica::from_snapshot`] で作る。
    fn new() -> Self;

    /// 試験で操作の列を再現するため、書き手IDを指定して空の文書を作る。
    ///
    /// 同じ文書を編集する複製どうしには、異なるIDを指定する。
    fn with_writer_id(writer_id: u32) -> Self;

    /// スナップショットから、別の書き手の複製を作る。
    fn from_snapshot(snapshot: &[u8]) -> Result<Self, ReplicaError>;

    /// 試験用の書き手IDで、スナップショットから複製を作る。
    ///
    /// `writer_id` は、スナップショット内の書き手と、ほかの複製の書き手のどちらとも重ねない。
    fn from_snapshot_with_writer_id(snapshot: &[u8], writer_id: u32) -> Result<Self, ReplicaError>;

    /// 今の本文を返す。
    fn text(&self) -> String;

    /// `pos` に `text` を挿入し、ほかの複製へ送る更新を返す。
    fn insert(&mut self, pos: usize, text: &str) -> Result<Vec<u8>, ReplicaError>;

    /// `pos` から `len` バイトを削除し、ほかの複製へ送る更新を返す。
    fn delete(&mut self, pos: usize, len: usize) -> Result<Vec<u8>, ReplicaError>;

    /// ほかの複製の更新を当てる。
    ///
    /// 更新は、順不同でも、同じ更新が重ねて届いてもよい。
    fn apply_update(&mut self, update: &[u8]) -> Result<(), ReplicaError>;

    /// ほかの複製の更新を当て、本文に起きた変更を返す。
    ///
    /// 前の更新が届いていない更新は、当たった分の変更だけを返す。
    fn apply_update_with_changes(&mut self, update: &[u8])
    -> Result<Vec<TextChange>, ReplicaError>;

    /// 履歴を含む、文書の全体のスナップショットを書き出す。
    fn snapshot(&mut self) -> Vec<u8>;

    /// 履歴をまとめたスナップショットを書き出す。まとめる入口が無い候補は `None` を返す。
    fn compacted_snapshot(&mut self) -> Option<Vec<u8>>;

    /// 今の時点を指す版を、バイト列で返す。
    fn version(&mut self) -> Vec<u8>;

    /// 版の時点の本文を返す。
    fn text_at(&mut self, version: &[u8]) -> Result<String, ReplicaError>;

    /// 今の文書から、別の書き手の分岐を作る。分岐は、本文とは別の文書として持つ。
    fn fork(&mut self) -> Result<Self, ReplicaError>;

    /// 分岐の編集を、この文書にマージする。
    fn merge(&mut self, branch: &mut Self) -> Result<(), ReplicaError>;

    /// 今の文書から、別の書き手の複製を作る。
    fn replicate(&mut self) -> Result<Self, ReplicaError> {
        Self::from_snapshot(&self.snapshot())
    }
}

/// 差分のイベントの1件。長さはUTF-8のバイト数で数える。
pub(crate) enum DeltaOp {
    Retain(usize),
    Delete(usize),
    Insert(String),
}

/// 先頭から順に並んだ差分のイベントを、変更の列にする。隣り合う削除と挿入は1件にまとめる。
pub(crate) fn changes_from_delta(ops: impl IntoIterator<Item = DeltaOp>) -> Vec<TextChange> {
    let mut changes: Vec<TextChange> = Vec::new();
    let mut pos = 0;
    for op in ops {
        let adjacent = changes
            .last_mut()
            .filter(|last| last.start + last.insert.len() == pos);
        match op {
            DeltaOp::Retain(len) => pos += len,
            DeltaOp::Delete(len) => match adjacent {
                Some(last) => last.end += len,
                None => changes.push(TextChange {
                    start: pos,
                    end: pos + len,
                    insert: String::new(),
                }),
            },
            DeltaOp::Insert(text) => {
                let len = text.len();
                match adjacent {
                    Some(last) => last.insert.push_str(&text),
                    None => changes.push(TextChange {
                        start: pos,
                        end: pos,
                        insert: text,
                    }),
                }
                pos += len;
            }
        }
    }
    changes
}
