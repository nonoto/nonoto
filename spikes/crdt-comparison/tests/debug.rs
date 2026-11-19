//! デバッグ表示が本文を出さないことを確かめる。

use std::fmt::Debug;

use crdt_comparison::data::japanese_markdown;
use crdt_comparison::{AutomergeReplica, LoroReplica, Replica, YrsReplica};

const BODY: &str = "デバッグ表示から伏せる本文";

fn assert_body_hidden<R: Replica + Debug>() {
    let mut replica = R::new();
    replica
        .insert(0, BODY)
        .expect("文書に文字列を挿入できなかった");

    assert!(
        !format!("{replica:?}").contains(BODY),
        "{}の複製のデバッグ表示に本文が含まれている",
        R::NAME
    );
}

#[test]
fn yrsの複製をデバッグ表示しても本文を出さない() {
    assert_body_hidden::<YrsReplica>();
}

#[test]
fn automergeの複製をデバッグ表示しても本文を出さない() {
    assert_body_hidden::<AutomergeReplica>();
}

#[test]
fn loroの複製をデバッグ表示しても本文を出さない() {
    assert_body_hidden::<LoroReplica>();
}

#[test]
fn 計測のデータをデバッグ表示しても本文を出さない() {
    let dataset = japanese_markdown(20, 1);
    let first_line = dataset.text.lines().next().expect("本文に行が無い");

    assert!(
        !format!("{dataset:?}").contains(first_line),
        "計測のデータのデバッグ表示に本文が含まれている"
    );
}
