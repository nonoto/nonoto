//! 収束と位置のテストで共有する部品。

use crdt_comparison::Replica;
pub use crdt_comparison::rng::SeededRng;

/// 最初の複製に `text` を入れ、そのスナップショットから、ほかの複製を作る。
pub fn replicas<R: Replica>(count: usize, text: &str) -> Vec<R> {
    let mut first = R::new();
    if !text.is_empty() {
        first.insert(0, text).expect("最初の本文を入れられなかった");
    }
    let mut replicas = Vec::with_capacity(count);
    for _ in 1..count {
        replicas.push(first.replicate().expect("複製を作れなかった"));
    }
    replicas.insert(0, first);
    replicas
}

/// `from` の複製が出した更新を、ほかのすべての複製に当てる。
pub fn broadcast<R: Replica>(replicas: &mut [R], from: usize, update: &[u8]) {
    for (i, replica) in replicas.iter_mut().enumerate() {
        if i != from {
            replica
                .apply_update(update)
                .expect("更新を当てられなかった");
        }
    }
}

/// すべての複製の本文が一致することを確かめ、その本文を返す。
pub fn assert_converged<R: Replica>(replicas: &[R]) -> String {
    let text = replicas[0].text();
    for (i, replica) in replicas.iter().enumerate().skip(1) {
        assert_eq!(
            replica.text(),
            text,
            "{}の複製{i}の本文が、複製0と一致しない",
            R::NAME
        );
    }
    text
}

/// `text` の中で、`needle` が始まるバイト位置を返す。
pub fn byte_pos(text: &str, needle: &str) -> usize {
    text.find(needle).expect("本文に、探す文字列が無い")
}
