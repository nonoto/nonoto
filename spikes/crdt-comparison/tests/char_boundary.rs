//! 文字の途中のバイト位置を渡したときの、候補ごとの扱いを確かめる。
//!
//! 扱いは候補によって異なるため、候補ごとに観測した挙動をテストにする。

use crdt_comparison::{AutomergeReplica, LoroReplica, Replica, ReplicaError};

/// 「あ」「い」は3バイト、「😀」は4バイトの文字。
const TEXT: &str = "あい😀";

fn replica_pair<R: Replica>() -> (R, R) {
    let mut a = R::new();
    a.insert(0, TEXT).unwrap();
    let b = a.replicate().unwrap();
    (a, b)
}

#[test]
fn loroは文字の途中の位置への挿入と削除をエラーにして本文を変えない() {
    let (mut a, _) = replica_pair::<LoroReplica>();
    assert_eq!(a.insert(1, "x"), Err(ReplicaError::Edit("loro")));
    assert_eq!(a.insert(8, "x"), Err(ReplicaError::Edit("loro")));
    assert_eq!(a.delete(1, 1), Err(ReplicaError::Edit("loro")));
    assert_eq!(a.delete(0, 1), Err(ReplicaError::Edit("loro")));
    assert_eq!(a.text(), TEXT);
}

#[test]
fn automergeは文字の途中の位置を後ろの文字の境界に丸める() {
    let (mut a, mut b) = replica_pair::<AutomergeReplica>();
    // 「あ」の途中 (1バイト目) への挿入は、「あ」の後ろに入る。
    let update = a.insert(1, "x").unwrap();
    assert_eq!(a.text(), "あxい😀");
    b.apply_update(&update).unwrap();
    assert_eq!(b.text(), a.text());

    // 「😀」の途中への挿入は、「😀」の後ろに入る。
    let (mut a, _) = replica_pair::<AutomergeReplica>();
    a.insert(8, "x").unwrap();
    assert_eq!(a.text(), "あい😀x");

    // 開始と長さも丸めるため、「あ」の途中から1バイトの削除は「い」を消す。
    let (mut a, _) = replica_pair::<AutomergeReplica>();
    a.delete(1, 1).unwrap();
    assert_eq!(a.text(), "あ😀");

    // 先頭から1バイトの削除は、「あ」を消す。
    let (mut a, _) = replica_pair::<AutomergeReplica>();
    a.delete(0, 1).unwrap();
    assert_eq!(a.text(), "い😀");
}

/// yrsは位置を検査せず、文字の途中の位置を渡すと、debugビルドでは整数の桁あふれでpanicする。
/// releaseビルドでは桁あふれが検査されず、位置がずれ、複製どうしの本文が食い違う。
#[cfg(debug_assertions)]
#[test]
fn yrsは文字の途中の位置を渡すとdebugビルドでpanicする() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use crdt_comparison::YrsReplica;

    // 挿入する位置と、削除する範囲 (開始と長さ)。
    for (pos, delete_len) in [(1, None), (8, None), (1, Some(1))] {
        let (mut a, _) = replica_pair::<YrsReplica>();
        let result = catch_unwind(AssertUnwindSafe(|| match delete_len {
            Some(len) => a.delete(pos, len),
            None => a.insert(pos, "x"),
        }));
        assert!(result.is_err(), "位置{pos}への編集がpanicしなかった");
    }
}
