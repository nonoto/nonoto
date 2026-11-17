//! 計測のデータが、決まった内容と形で作られることを確かめる。

use crdt_comparison::data::{Edit, japanese_markdown};
use crdt_comparison::measure::build;
use crdt_comparison::{AutomergeReplica, LoroReplica, Replica, YrsReplica};

#[test]
fn 同じ種からは同じ入力の列と本文ができる() {
    let a = japanese_markdown(300, 7);
    let b = japanese_markdown(300, 7);
    assert_eq!(a.edits, b.edits);
    assert_eq!(a.text, b.text);
    assert_ne!(a.text, japanese_markdown(300, 8).text);
}

#[test]
fn 指定した行数の本文を入力の列から作る() {
    let dataset = japanese_markdown(500, 2026);
    assert_eq!(dataset.lines(), 500);

    let mut text = String::new();
    for edit in &dataset.edits {
        match edit {
            Edit::Insert { pos, text: s } => text.insert_str(*pos, s),
            Edit::Delete { pos, len } => text.replace_range(*pos..*pos + *len, ""),
        }
    }
    assert_eq!(text, dataset.text);
}

#[test]
fn 入力の列は1文字の入力と変換の確定と打ち直しと貼り付けを含む() {
    let dataset = japanese_markdown(500, 2026);
    let inserts: Vec<&String> = dataset
        .edits
        .iter()
        .filter_map(|edit| match edit {
            Edit::Insert { text, .. } => Some(text),
            Edit::Delete { .. } => None,
        })
        .collect();
    let chars = |s: &str| s.chars().count();
    assert!(inserts.iter().any(|s| chars(s) == 1), "1文字の入力が無い");
    assert!(
        inserts
            .iter()
            .any(|s| (2..=6).contains(&chars(s)) && !s.is_ascii()),
        "IMEの確定に当たる挿入が無い"
    );
    assert!(
        inserts.iter().any(|s| s.matches('\n').count() >= 10),
        "貼り付けが無い"
    );
    assert!(
        dataset
            .edits
            .iter()
            .any(|edit| matches!(edit, Edit::Delete { .. })),
        "打ち直しの削除が無い"
    );
    assert!(dataset.text.contains("## "), "見出しが無い");
    assert!(dataset.text.contains("[["), "Wikiリンクが無い");
}

#[test]
fn wikiリンクの題名は記号と分けてimeで数文字ずつ確定する() {
    let dataset = japanese_markdown(500, 2026);
    let inserts: Vec<&str> = dataset
        .edits
        .iter()
        .filter_map(|edit| match edit {
            Edit::Insert { text, .. } => Some(text.as_str()),
            Edit::Delete { .. } => None,
        })
        .collect();
    let mut links = 0;
    for window in inserts.windows(3) {
        if window[0] == "[" && window[1] == "[" {
            links += 1;
            let title = window[2];
            assert!(
                !title.is_ascii() && title.chars().count() >= 2,
                "`[[` のあとの題名が、IMEで確定する数文字のまとまりになっていない: {title:?}"
            );
        }
    }
    assert!(links > 0, "1文字ずつ打ったWikiリンクが無い");
}

fn 入力の列から作った文書の本文が一致する<R: Replica>() {
    let dataset = japanese_markdown(200, 3);
    let doc = build::<R>(&dataset).unwrap();
    assert_eq!(doc.text(), dataset.text, "{}", R::NAME);
}

#[test]
fn yrsで入力の列から作った文書の本文が一致する() {
    入力の列から作った文書の本文が一致する::<YrsReplica>();
}

#[test]
fn automergeで入力の列から作った文書の本文が一致する() {
    入力の列から作った文書の本文が一致する::<AutomergeReplica>();
}

#[test]
fn loroで入力の列から作った文書の本文が一致する() {
    入力の列から作った文書の本文が一致する::<LoroReplica>();
}
