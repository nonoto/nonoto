//! ページ1件の本文をCRDTで持つ文書。
//!
//! 位置は、入力の部品と同じくUTF-8のバイト位置で扱う。
//! CRDTのライブラリ (Loro) の型は公開APIに出さず、更新とスナップショットはバイト列を包んだ型で渡す。

use std::fmt;
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use loro::event::{Diff, DiffEvent};
use loro::{ContainerTrait, EncodedBlobMode, ExportMode, LoroDoc, LoroText, TextDelta};

use crate::error::Error;

/// 本文を入れるルートのコンテナの名前。ルートのコンテナは名前で決まるため、どの複製でも同じコンテナを指す。
const BODY: &str = "body";

/// ほかの端末へ送る、文書の変更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update(Vec<u8>);

impl Update {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// 履歴を含む文書の全体。同じ文書を別の端末で開くときに渡す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot(Vec<u8>);

impl Snapshot {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// 本文の1か所の置き換え。`range` を `text` に置き換える。
///
/// 置き換えの列は先頭から順に当て、各置き換えの範囲は、その前の置き換えを当てたあとの本文で数える。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pub range: Range<usize>,
    pub text: String,
}

impl Replacement {
    /// 置き換えの列を本文に当てる。
    pub fn apply_all(body: &str, replacements: &[Replacement]) -> String {
        let mut body = body.to_owned();
        for replacement in replacements {
            body.replace_range(replacement.range.clone(), &replacement.text);
        }
        body
    }
}

/// 位置にちょうど挿入があったときに、位置を挿入の前と後ろのどちらに置くか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    Before,
    After,
}

/// 位置を、置き換えの列を当てたあとの本文の位置に写す。
///
/// 置き換えで消えた範囲の中の位置は、`affinity` に従って、置き換えた文字列の前か後ろに置く。
pub fn map_position(mut pos: usize, replacements: &[Replacement], affinity: Affinity) -> usize {
    for replacement in replacements {
        let Range { start, end } = replacement.range;
        let inserted = replacement.text.len();
        pos = if pos < start || (pos == start && affinity == Affinity::Before) {
            pos
        } else if pos >= end && pos > start {
            pos - (end - start) + inserted
        } else {
            match affinity {
                Affinity::Before => start,
                Affinity::After => start + inserted,
            }
        };
    }
    pos
}

/// ページ1件の本文を持つ文書。
///
/// デバッグ表示では本文を伏せる。
pub struct Document {
    doc: LoroDoc,
    text: LoroText,
    /// 今の本文。Loroの差分はコードポイントで数えるため、届いた差分をバイト位置にするのにも使う。
    body: String,
}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document").finish_non_exhaustive()
    }
}

impl Document {
    /// 本文の文字列から、新しい文書を作る。
    ///
    /// 同じ文字列から別々に作った文書どうしは、同じ文書にならない。
    /// 同じ文書を開くには、[`Document::snapshot`] を [`Document::from_snapshot`] で読み込む。
    pub fn new(body: &str) -> Result<Self, Error> {
        let mut document = Self::with_doc(LoroDoc::new());
        if !body.is_empty() {
            document.replace(0..0, body)?;
        }
        Ok(document)
    }

    /// スナップショットから、別の書き手の文書を作る。
    ///
    /// この文書が出力する形式で、履歴をすべて持つものを受け付ける。
    pub fn from_snapshot(snapshot: &Snapshot) -> Result<Self, Error> {
        let meta = LoroDoc::decode_import_blob_meta(snapshot.as_bytes(), true)
            .map_err(|_| Error::InvalidSnapshot)?;
        if meta.mode != EncodedBlobMode::Snapshot {
            return Err(Error::InvalidSnapshot);
        }
        let doc =
            LoroDoc::from_snapshot(snapshot.as_bytes()).map_err(|_| Error::InvalidSnapshot)?;
        if doc.is_shallow() {
            return Err(Error::InvalidSnapshot);
        }
        Ok(Self::with_doc(doc))
    }

    fn with_doc(doc: LoroDoc) -> Self {
        let text = doc.get_text(BODY);
        let body = text.to_string();
        Self { doc, text, body }
    }

    /// 今の本文を返す。
    pub fn text(&self) -> &str {
        &self.body
    }

    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        self.doc
            .export(ExportMode::Snapshot)
            .map(Snapshot)
            .map_err(|_| Error::DocumentExport)
    }

    /// 手元の編集を当て、ほかの端末へ送る更新を返す。本文が変わらない編集では `None` を返す。
    ///
    /// `range` の今の本文と `text` で共通する前と後ろを除き、変わった箇所だけを置き換える。
    /// 範囲が本文の外にあるか、文字の途中を指すときはエラーにし、文書を変えない。
    pub fn apply_local(
        &mut self,
        range: Range<usize>,
        text: &str,
    ) -> Result<Option<Update>, Error> {
        check_range(&self.body, &range)?;
        let minimal = minimize(&self.body, range, text);
        if minimal.range.is_empty() && minimal.text.is_empty() {
            return Ok(None);
        }
        self.replace(minimal.range, &minimal.text).map(Some)
    }

    /// 文字の境界にある範囲を置き換え、その変更を更新として取り出す。
    fn replace(&mut self, range: Range<usize>, text: &str) -> Result<Update, Error> {
        let before = self.doc.oplog_vv();
        if !range.is_empty() {
            self.text
                .delete_utf8(range.start, range.len())
                .map_err(|_| Error::InvalidRange)?;
        }
        if !text.is_empty() {
            self.text
                .insert_utf8(range.start, text)
                .map_err(|_| Error::InvalidRange)?;
        }
        self.doc.commit();
        self.body.replace_range(range, text);
        self.doc
            .export(ExportMode::updates(&before))
            .map(Update)
            .map_err(|_| Error::DocumentExport)
    }

    /// ほかの端末の更新を当て、本文に起きた変更を置き換えの列で返す。
    ///
    /// 更新は、順不同でも、同じ更新が重ねて届いてもよい。
    /// 前の更新が届いていない更新は文書の中に保留され、前の更新が届いたときに、両方の変更を返す。
    /// この文書が出力する更新の形式を受け付け、形式が違う入力では文書を変えない。
    pub fn apply_remote(&mut self, update: &Update) -> Result<Vec<Replacement>, Error> {
        let meta = LoroDoc::decode_import_blob_meta(update.as_bytes(), true)
            .map_err(|_| Error::InvalidUpdate)?;
        if meta.mode != EncodedBlobMode::Updates {
            return Err(Error::InvalidUpdate);
        }
        let ops = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&ops);
        let subscription = self.doc.subscribe(
            &self.text.id(),
            Arc::new(move |event: DiffEvent| {
                let mut sink = sink.lock().unwrap_or_else(PoisonError::into_inner);
                for container in event.events {
                    if let Diff::Text(deltas) = container.diff {
                        sink.extend(deltas);
                    }
                }
            }),
        );
        let result = self.doc.import(update.as_bytes());
        drop(subscription);
        result.map_err(|_| Error::InvalidUpdate)?;

        let deltas = std::mem::take(&mut *ops.lock().unwrap_or_else(PoisonError::into_inner));
        let replacements = replacements_from_deltas(&self.body, deltas);
        self.body = Replacement::apply_all(&self.body, &replacements);
        Ok(replacements)
    }
}

/// 範囲が本文の中にあり、両端が文字の境界にあるかを確かめる。
fn check_range(body: &str, range: &Range<usize>) -> Result<(), Error> {
    if range.start <= range.end
        && range.end <= body.len()
        && body.is_char_boundary(range.start)
        && body.is_char_boundary(range.end)
    {
        Ok(())
    } else {
        Err(Error::InvalidRange)
    }
}

/// `range` の今の本文と `text` で共通する前と後ろを除いた置き換えを返す。
///
/// 除く長さは文字の境界に合わせる。
/// 同じ文字が続く箇所では、前の部分を先に長く取るため、挿入と削除は続いた文字の後ろの端に寄る。
fn minimize(body: &str, range: Range<usize>, text: &str) -> Replacement {
    let old = &body[range.clone()];
    let prefix = old
        .char_indices()
        .zip(text.chars())
        .find(|((_, a), b)| a != b)
        .map_or_else(|| old.len().min(text.len()), |((i, _), _)| i);
    let (old_rest, text_rest) = (&old[prefix..], &text[prefix..]);
    let suffix: usize = old_rest
        .chars()
        .rev()
        .zip(text_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    Replacement {
        range: range.start + prefix..range.end - suffix,
        text: text_rest[..text_rest.len() - suffix].to_owned(),
    }
}

/// Loroの差分 (保持と削除の長さはコードポイント) を、`body` の上でバイト位置の置き換えの列にする。
///
/// 保持と削除は変更の前の本文で数えるため、`body` を先頭から読み進めてバイト数にする。
/// 隣り合う削除と挿入は、1件の置き換えにまとめる。
fn replacements_from_deltas(body: &str, deltas: Vec<TextDelta>) -> Vec<Replacement> {
    let mut replacements: Vec<Replacement> = Vec::new();
    // 変更の前の本文で読んだ位置と、置き換えを当てたあとの本文での位置。
    let mut old = 0;
    let mut new = 0;
    for delta in deltas {
        let adjacent = replacements
            .last_mut()
            .filter(|last| last.range.start + last.text.len() == new);
        match delta {
            TextDelta::Retain { retain, .. } => {
                let len = byte_len_of_chars(body, old, retain);
                old += len;
                new += len;
            }
            TextDelta::Delete { delete } => {
                let len = byte_len_of_chars(body, old, delete);
                old += len;
                match adjacent {
                    Some(last) => last.range.end += len,
                    None => replacements.push(Replacement {
                        range: new..new + len,
                        text: String::new(),
                    }),
                }
            }
            TextDelta::Insert { insert, .. } => {
                let len = insert.len();
                match adjacent {
                    Some(last) => last.text.push_str(&insert),
                    None => replacements.push(Replacement {
                        range: new..new,
                        text: insert,
                    }),
                }
                new += len;
            }
        }
    }
    replacements
}

/// `text` の `from` バイト目から `chars` 文字分のバイト数を返す。
fn byte_len_of_chars(text: &str, from: usize, chars: usize) -> usize {
    text[from..]
        .char_indices()
        .nth(chars)
        .map_or(text.len() - from, |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 漢字・かな・絵文字 (サロゲートペア・ZWJでつないだ絵文字・国旗)・結合文字・CRLFを含む本文。
    const MIXED: &str = "漢字とかな😀👨\u{200D}👩\u{200D}👧🇯🇵\r\nか\u{3099}が\r\n";

    fn pair(body: &str) -> (Document, Document) {
        let a = Document::new(body).expect("文書を作れなかった");
        let b = Document::from_snapshot(&a.snapshot().expect("スナップショットを出力できなかった"))
            .expect("スナップショットから文書を作れなかった");
        (a, b)
    }

    fn pos(body: &str, needle: &str) -> usize {
        body.find(needle).expect("本文に、探す文字列が無い")
    }

    /// 全体の置き換えを、最小にせずにそのまま当てたときの更新。比べるためだけに使う。
    fn naive_full_replace(body: &str, text: &str) -> Update {
        let mut document = Document::from_snapshot(
            &Document::new(body)
                .expect("文書を作れなかった")
                .snapshot()
                .expect("スナップショットを出力できなかった"),
        )
        .expect("スナップショットから文書を作れなかった");
        let len = document.text().len();
        document
            .replace(0..len, text)
            .expect("本文を置き換えられなかった")
    }

    #[test]
    fn 本文の文字列から作った文書はその本文を持つ() {
        let document = Document::new(MIXED).expect("文書を作れなかった");
        assert_eq!(document.text(), MIXED);
        assert_eq!(Document::new("").expect("文書を作れなかった").text(), "");
    }

    #[test]
    fn 文書をデバッグ表示しても本文を出さない() {
        let body = "デバッグ表示から伏せる本文";
        let document = Document::new(body).expect("文書を作れなかった");

        assert!(
            !format!("{document:?}").contains(body),
            "文書のデバッグ表示に本文が含まれている"
        );
    }

    #[test]
    fn スナップショットから読み込んだ文書は同じ本文を持ち同じ文書として編集できる() {
        let (mut a, mut b) = pair(MIXED);
        assert_eq!(b.text(), MIXED);
        let update = b
            .apply_local(0..0, "前")
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わる編集なのに更新が出なかった");
        a.apply_remote(&update).expect("更新を当てられなかった");
        assert_eq!(a.text(), format!("前{MIXED}"));
    }

    #[test]
    fn 本文の全体を置き換える編集は変わった箇所だけの更新になる() {
        let body = "あいうえお\n".repeat(200);
        let (mut a, mut b) = pair(&body);
        // IMEの確定のように、本文の全体を「変換を始める前の本文に確定した文字を足したもの」で置き換える。
        let at = pos(&body, "う");
        let mut committed = body.clone();
        committed.insert_str(at, "漢字");

        let update = a
            .apply_local(0..body.len(), &committed)
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わる編集なのに更新が出なかった");
        let naive = naive_full_replace(&body, &committed);
        assert!(
            update.as_bytes().len() * 10 < naive.as_bytes().len(),
            "最小の置き換えの更新 ({}B) が、全体の置き換えの更新 ({}B) より十分に小さくない",
            update.as_bytes().len(),
            naive.as_bytes().len()
        );

        let replacements = b.apply_remote(&update).expect("更新を当てられなかった");
        assert_eq!(
            replacements,
            vec![Replacement {
                range: at..at,
                text: "漢字".to_owned()
            }]
        );
        assert_eq!(b.text(), committed);
    }

    #[test]
    fn 置き換えの前後の共通部分は文字の境界で除く() {
        // 「😀」と「😃」はUTF-8の先頭の3バイトが同じで、最後のバイトだけが違う。
        let minimal = minimize("あ😀い", 0.."あ😀い".len(), "あ😃い");
        assert_eq!(
            minimal,
            Replacement {
                range: "あ".len().."あ😀".len(),
                text: "😃".to_owned()
            }
        );
    }

    #[test]
    fn 同じ文字が続く箇所への挿入は続いた文字の後ろの端に寄る() {
        let minimal = minimize("ああ", 0.."ああ".len(), "あああ");
        assert_eq!(
            minimal,
            Replacement {
                range: "ああ".len().."ああ".len(),
                text: "あ".to_owned()
            }
        );
    }

    #[test]
    fn 離れた複数の箇所が変わる置き換えはそれらを含む1つの置き換えになる() {
        let minimal = minimize("あいうえお", 0.."あいうえお".len(), "かいうえこ");
        assert_eq!(
            minimal,
            Replacement {
                range: 0.."あいうえお".len(),
                text: "かいうえこ".to_owned()
            }
        );
    }

    #[test]
    fn 離れた2か所を1回で変えると間の文字列が消して入れ直され相手の挿入が元の位置に残らない() {
        let (mut a, mut b) = pair("一二三四五");
        // aは「二」を消し、「四」の後ろに挿入する。2か所を1回の置き換えにすると「二三四」を「三四挿」にする置き換えになる。
        let update_a = a
            .apply_local(0.."一二三四五".len(), "一三四挿五")
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わる編集なのに更新が出なかった");
        // bは同時に、「三」の前に挿入する。
        let at = pos(b.text(), "三");
        let update_b = b
            .apply_local(at..at, "割")
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わる編集なのに更新が出なかった");
        a.apply_remote(&update_b).expect("更新を当てられなかった");
        b.apply_remote(&update_a).expect("更新を当てられなかった");

        assert_eq!(a.text(), b.text());
        // 「三」は消して入れ直されるため、bの挿入は入れ直した「三」の前に付かず、置き換えの前か後ろに寄る。
        // どちらに寄るかは書き手のIDの順で決まる。
        assert!(
            ["一割三四挿五", "一三四挿割五"].contains(&a.text()),
            "{:?}",
            a.text()
        );
    }

    #[test]
    fn 本文が変わらない編集は更新を出さない() {
        let mut document = Document::new("あいう").expect("文書を作れなかった");
        assert_eq!(
            document
                .apply_local(0.."あいう".len(), "あいう")
                .expect("手元の編集を当てられなかった"),
            None
        );
        assert_eq!(
            document
                .apply_local(3..3, "")
                .expect("手元の編集を当てられなかった"),
            None
        );
    }

    #[test]
    fn 本文の外や文字の途中を指す範囲はエラーにして文書を変えない() {
        let mut document = Document::new("あい").expect("文書を作れなかった");
        let before = document
            .snapshot()
            .expect("スナップショットを出力できなかった");
        // 終わりが始まりより前の範囲も、エラーにする。
        let reversed = Range { start: 4, end: 3 };
        for range in [1..1, 0..1, 3..4, 0..7, reversed] {
            assert!(
                matches!(
                    document.apply_local(range.clone(), "x"),
                    Err(Error::InvalidRange)
                ),
                "範囲 {range:?} がエラーにならなかった"
            );
            assert_eq!(document.text(), "あい");
            assert_eq!(
                document
                    .snapshot()
                    .expect("スナップショットを出力できなかった"),
                before
            );
        }
    }

    #[test]
    fn 日本語を含む同時の編集が収束する() {
        let (mut a, mut b) = pair(MIXED);
        let mut updates_a = Vec::new();
        let mut updates_b = Vec::new();

        // aは編集のたびに本文の全体を置き換える形で、ZWJでつないだ絵文字を消し、CRLFの直後に挿入する。
        let family = "👨\u{200D}👩\u{200D}👧";
        let next = a.text().replacen(family, "", 1);
        let len = a.text().len();
        updates_a.push(
            a.apply_local(0..len, &next)
                .expect("手元の編集を当てられなかった")
                .expect("本文が変わる編集なのに更新が出なかった"),
        );
        let mut next = a.text().to_owned();
        next.insert(pos(&next, "\r\n") + 2, '行');
        let len = a.text().len();
        updates_a.push(
            a.apply_local(0..len, &next)
                .expect("手元の編集を当てられなかった")
                .expect("本文が変わる編集なのに更新が出なかった"),
        );

        // bは結合文字の濁点だけを消し、国旗の前と先頭に挿入する。
        let at = pos(b.text(), "\u{3099}");
        updates_b.push(
            b.apply_local(at..at + "\u{3099}".len(), "")
                .expect("手元の編集を当てられなかった")
                .expect("本文が変わる編集なのに更新が出なかった"),
        );
        let at = pos(b.text(), "🇯🇵");
        updates_b.push(
            b.apply_local(at..at, "🇺🇸")
                .expect("手元の編集を当てられなかった")
                .expect("本文が変わる編集なのに更新が出なかった"),
        );
        updates_b.push(
            b.apply_local(0..0, "# ")
                .expect("手元の編集を当てられなかった")
                .expect("本文が変わる編集なのに更新が出なかった"),
        );

        // 順を入れ替えて届ける。
        for update in updates_b.iter().rev() {
            a.apply_remote(update).expect("更新を当てられなかった");
        }
        for update in &updates_a {
            b.apply_remote(update).expect("更新を当てられなかった");
        }
        assert_eq!(a.text(), b.text());
        assert_eq!(a.text(), "# 漢字とかな😀🇺🇸🇯🇵\r\n行かが\r\n");
    }

    #[derive(Debug, PartialEq, Eq)]
    enum SessionEvent {
        Edit {
            replica: usize,
            replacement: Replacement,
        },
        Deliver {
            replica: usize,
            update: usize,
        },
    }

    #[derive(Debug, PartialEq, Eq)]
    struct EditSession {
        writers: [u64; 2],
        events: Vec<SessionEvent>,
        body: String,
    }

    fn run_edit_session(seed: u64) -> EditSession {
        // 初期本文の挿入も含めて再現するため、書き手は本文を入れる前に固定する。
        const FRAGMENTS: &[&str] = &["あ", "漢字", "😀", "👨\u{200D}👩\u{200D}👧", "\r\n", "a"];
        // 2倍しても溢れないよう、種の下位32ビットから書き手のIDを決める。
        let base = (seed & u64::from(u32::MAX)) * 2;
        let doc = LoroDoc::new();
        doc.set_peer_id(base).expect("書き手のIDを設定できなかった");
        let mut a = Document::with_doc(doc);
        a.replace(0..0, MIXED).expect("本文を置き換えられなかった");
        let mut b =
            Document::from_snapshot(&a.snapshot().expect("スナップショットを出力できなかった"))
                .expect("スナップショットから文書を作れなかった");
        b.doc
            .set_peer_id(base + 1)
            .expect("書き手のIDを設定できなかった");
        let writers = [a.doc.peer_id(), b.doc.peer_id()];
        let mut events = Vec::new();
        let mut next_update = 0;
        let mut state = seed;
        let mut next = |n: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize % n
        };
        for step in 0..200 {
            let body = b.text().to_owned();
            let boundaries: Vec<usize> = body
                .char_indices()
                .map(|(i, _)| i)
                .chain([body.len()])
                .collect();
            let start = boundaries[next(boundaries.len())];
            let end = boundaries
                .iter()
                .copied()
                .filter(|&i| i >= start)
                .nth(next(4))
                .unwrap_or(start);
            let text = FRAGMENTS[next(FRAGMENTS.len())];
            events.push(SessionEvent::Edit {
                replica: 1,
                replacement: Replacement {
                    range: start..end,
                    text: text.to_owned(),
                },
            });
            let Some(update) = b
                .apply_local(start..end, text)
                .expect("手元の編集を当てられなかった")
            else {
                continue;
            };
            let remote_id = next_update;
            next_update += 1;
            // 手元の側の編集も混ぜ、手元の編集が置き換えの列に入らないことを確かめる。
            if next(3) == 0 {
                let len = a.text().len();
                events.push(SessionEvent::Edit {
                    replica: 0,
                    replacement: Replacement {
                        range: len..len,
                        text: "手".to_owned(),
                    },
                });
                let local = a
                    .apply_local(len..len, "手")
                    .expect("手元の編集を当てられなかった")
                    .expect("本文が変わる編集なのに更新が出なかった");
                b.apply_remote(&local).expect("更新を当てられなかった");
                events.push(SessionEvent::Deliver {
                    replica: 1,
                    update: next_update,
                });
                next_update += 1;
            }
            let before = a.text().to_owned();
            let replacements = a.apply_remote(&update).expect("更新を当てられなかった");
            events.push(SessionEvent::Deliver {
                replica: 0,
                update: remote_id,
            });
            assert_eq!(
                Replacement::apply_all(&before, &replacements),
                a.text(),
                "手{step}の置き換えの列 {replacements:?}"
            );
            assert_eq!(a.text(), a.text.to_string(), "手{step}で本文の写しがずれた");
        }
        assert_eq!(a.text(), b.text());
        EditSession {
            writers,
            events,
            body: a.text().to_owned(),
        }
    }

    #[test]
    fn 届いた更新の置き換えの列を古い本文に当てると新しい本文になり操作と配送を再現できる() {
        for seed in [7, 42, 2026] {
            assert_eq!(
                run_edit_session(seed),
                run_edit_session(seed),
                "種{seed}の書き手と操作と配送を再現できない"
            );
        }
    }

    #[test]
    fn 前の更新が届いていない更新は保留され前の更新が届いたときに両方の変更を返す() {
        let (mut a, mut b) = pair("あいう");
        let first = b
            .apply_local(0..0, "前")
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わる編集なのに更新が出なかった");
        let second = b
            .apply_local(0..0, "後")
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わる編集なのに更新が出なかった");

        assert_eq!(
            a.apply_remote(&second).expect("更新を当てられなかった"),
            Vec::new()
        );
        assert_eq!(a.text(), "あいう");
        let replacements = a.apply_remote(&first).expect("更新を当てられなかった");
        assert_eq!(
            Replacement::apply_all("あいう", &replacements),
            "後前あいう"
        );
        assert_eq!(a.text(), "後前あいう");
        // 同じ更新を重ねて当てても変わらない。
        assert_eq!(
            a.apply_remote(&first).expect("更新を当てられなかった"),
            Vec::new()
        );
        assert_eq!(a.text(), "後前あいう");
    }

    #[test]
    fn 壊れた更新とスナップショットはエラーになる() {
        let mut document = Document::new("あいう").expect("文書を作れなかった");
        assert!(matches!(
            document.apply_remote(&Update::from_bytes(vec![1, 2, 3])),
            Err(Error::InvalidUpdate)
        ));
        assert_eq!(document.text(), "あいう");
        assert!(matches!(
            Document::from_snapshot(&Snapshot::from_bytes(vec![1, 2, 3])),
            Err(Error::InvalidSnapshot)
        ));
    }

    #[test]
    fn 履歴を省略したスナップショットを拒否し完全な履歴では同じ更新を適用できる() {
        let author = LoroDoc::new();
        author.set_peer_id(1).expect("書き手のIDを設定できなかった");
        let text = author.get_text(BODY);
        text.insert(0, "a")
            .expect("Loroの文書に文字列を挿入できなかった");
        author.commit();
        let old = author.oplog_frontiers();
        text.insert(1, "b")
            .expect("Loroの文書に文字列を挿入できなかった");
        author.commit();
        let full = Snapshot(
            author
                .export(ExportMode::Snapshot)
                .expect("Loroの文書を出力できなかった"),
        );
        let shallow = Snapshot(
            author
                .export(ExportMode::shallow_snapshot(&author.oplog_frontiers()))
                .expect("Loroの文書を出力できなかった"),
        );
        assert!(matches!(
            Document::from_snapshot(&shallow),
            Err(Error::InvalidSnapshot)
        ));

        // 履歴を省略した文書では、一部の変更を当てたあとにエラーになる更新を作る。
        text.insert(2, "c")
            .expect("Loroの文書に文字列を挿入できなかった");
        author.commit();
        let other = LoroDoc::from_snapshot(
            &author
                .export(ExportMode::snapshot_at(&old))
                .expect("Loroの文書を出力できなかった"),
        )
        .expect("スナップショットからLoroの文書を作れなかった");
        other.set_peer_id(2).expect("書き手のIDを設定できなかった");
        other
            .get_text(BODY)
            .insert(0, "x")
            .expect("Loroの文書に文字列を挿入できなかった");
        other.commit();
        author
            .import(
                &other
                    .export(ExportMode::all_updates())
                    .expect("Loroの文書を出力できなかった"),
            )
            .expect("Loroの文書に更新を取り込めなかった");
        let update = Update(
            author
                .export(ExportMode::all_updates())
                .expect("Loroの文書を出力できなかった"),
        );
        let mut document =
            Document::from_snapshot(&full).expect("スナップショットから文書を作れなかった");
        let replacements = document
            .apply_remote(&update)
            .expect("更新を当てられなかった");
        assert_eq!(Replacement::apply_all("ab", &replacements), "xabc");
        assert_eq!(document.text(), "xabc");
        assert_eq!(
            Document::from_snapshot(
                &document
                    .snapshot()
                    .expect("スナップショットを出力できなかった")
            )
            .expect("スナップショットから文書を作れなかった")
            .text(),
            document.text()
        );
    }

    #[test]
    fn 更新の型で渡されたスナップショットを拒否しても文書は変わらず編集できる() {
        let source = LoroDoc::new();
        source
            .get_text(BODY)
            .insert(0, "別の本文")
            .expect("Loroの文書に文字列を挿入できなかった");
        source.commit();
        let full = source
            .export(ExportMode::Snapshot)
            .expect("Loroの文書を出力できなかった");
        let shallow = source
            .export(ExportMode::shallow_snapshot(&source.oplog_frontiers()))
            .expect("Loroの文書を出力できなかった");
        for body in ["", MIXED] {
            let mut document = Document::new(body).expect("文書を作れなかった");
            let before = document
                .snapshot()
                .expect("スナップショットを出力できなかった");
            for snapshot in [&full, &shallow] {
                assert!(matches!(
                    document.apply_remote(&Update::from_bytes(snapshot.clone())),
                    Err(Error::InvalidUpdate)
                ));
                assert_eq!(document.text(), body);
                assert_eq!(
                    document
                        .snapshot()
                        .expect("スナップショットを出力できなかった"),
                    before
                );
            }
            let update = document
                .apply_local(0..0, "続き")
                .expect("手元の編集を当てられなかった")
                .expect("本文が変わる編集なのに更新が出なかった");
            let mut other =
                Document::from_snapshot(&before).expect("スナップショットから文書を作れなかった");
            other.apply_remote(&update).expect("更新を当てられなかった");
            assert_eq!(document.text(), format!("続き{body}"));
            assert_eq!(document.text(), other.text());
        }
    }

    #[test]
    fn スナップショットの型で渡された更新を拒否する() {
        let mut document = Document::new("本文").expect("文書を作れなかった");
        let update = document
            .apply_local(0..0, "前")
            .expect("手元の編集を当てられなかった")
            .expect("本文が変わる編集なのに更新が出なかった");
        assert!(matches!(
            Document::from_snapshot(&Snapshot::from_bytes(update.as_bytes().to_vec())),
            Err(Error::InvalidSnapshot)
        ));
    }

    #[test]
    fn 位置を置き換えの列を越えて写す() {
        let body = "あいうえお";
        let i = pos(body, "い");
        let u = pos(body, "う");
        let e = pos(body, "え");
        let replacements = vec![
            // 「い」の前に挿入する。
            Replacement {
                range: i..i,
                text: "😀".to_owned(),
            },
            // 挿入のあとの本文で、「う」を「ウウ」に置き換える。
            Replacement {
                range: u + "😀".len()..e + "😀".len(),
                text: "ウウ".to_owned(),
            },
        ];
        let after = Replacement::apply_all(body, &replacements);
        assert_eq!(after, "あ😀いウウえお");

        // 置き換えより前の位置は動かない。
        assert_eq!(map_position(0, &replacements, Affinity::Before), 0);
        // 置き換えより後ろの位置は、同じ文字の前に留まる。
        assert_eq!(
            map_position(e, &replacements, Affinity::Before),
            pos(&after, "え")
        );
        assert_eq!(
            map_position(body.len(), &replacements, Affinity::After),
            after.len()
        );
        // 挿入と同じ位置は、向きに従って挿入の前か後ろに置く。
        assert_eq!(map_position(i, &replacements, Affinity::Before), i);
        assert_eq!(
            map_position(i, &replacements, Affinity::After),
            pos(&after, "い")
        );
        // 置き換えで消えた範囲の中の位置は、置き換えた文字列の前か後ろに置く。
        let middle_of_u = u;
        assert_eq!(
            map_position(middle_of_u, &replacements, Affinity::Before),
            pos(&after, "ウ")
        );
        assert_eq!(
            map_position(middle_of_u, &replacements, Affinity::After),
            pos(&after, "え")
        );
    }

    #[test]
    fn 消えた範囲の終わりの位置は置き換えた文字列の後ろに置く() {
        let replacements = vec![Replacement {
            range: 3..9,
            text: "x".to_owned(),
        }];
        assert_eq!(map_position(9, &replacements, Affinity::Before), 4);
        assert_eq!(map_position(6, &replacements, Affinity::Before), 3);
        assert_eq!(map_position(6, &replacements, Affinity::After), 4);
        assert_eq!(map_position(3, &replacements, Affinity::Before), 3);
    }
}
