//! 版と分岐、届いた更新の変更の列を、すべての候補で確かめる。

mod common;

use common::{SeededRng, assert_converged, broadcast, byte_pos, replicas};
use crdt_comparison::{Replica, TextChange};

/// 漢字・かな・絵文字・結合文字・CRLFを含む本文。
const MIXED_TEXT: &str = "漢字とかな😀👨\u{200D}👩\u{200D}👧🇯🇵\r\nか\u{3099}が\r\n";

fn 編集の途中で取った版の本文を別の複製で取り出せる<R: Replica>() {
    let mut author = R::new();
    let mut versions = Vec::new();

    author.insert(0, MIXED_TEXT).unwrap();
    versions.push((author.version(), author.text()));

    let pos = byte_pos(&author.text(), "かな");
    author.delete(pos, "かな".len()).unwrap();
    author.insert(0, "# 見出し\r\n").unwrap();
    versions.push((author.version(), author.text()));

    let len = author.text().len();
    author.delete(0, len).unwrap();
    author.insert(0, "書き直した本文").unwrap();
    versions.push((author.version(), author.text()));

    // 版を取ったあとの更新まで受け取った、別の書き手の複製。
    let mut reader = R::from_snapshot(&author.snapshot()).unwrap();
    for (i, (version, expected)) in versions.iter().enumerate() {
        assert!(!version.is_empty(), "{}: 版{i}が空のバイト列", R::NAME);
        assert_eq!(
            &author.text_at(version).unwrap(),
            expected,
            "{}: 版{i}",
            R::NAME
        );
        assert_eq!(
            &reader.text_at(version).unwrap(),
            expected,
            "{}: 別の複製での版{i}",
            R::NAME
        );
    }
}

fn 同時の編集をマージしたあとの版の本文を取り出せる<R: Replica>() {
    let mut rs = replicas::<R>(3, "一二三");
    let updates: Vec<(usize, Vec<u8>)> = (0..rs.len())
        .map(|i| (i, rs[i].insert(0, ["甲", "乙", "丙"][i]).unwrap()))
        .collect();
    for (from, update) in &updates {
        broadcast(&mut rs, *from, update);
    }
    let version = rs[0].version();
    let expected = assert_converged(&rs);
    rs[1].insert(0, "後").unwrap();

    assert_eq!(rs[1].text_at(&version).unwrap(), expected, "{}", R::NAME);
    assert_eq!(rs[2].text_at(&version).unwrap(), expected, "{}", R::NAME);
}

fn 分岐を別々に編集してマージすると両方の編集が入る<R: Replica>() {
    let mut main = R::new();
    main.insert(0, MIXED_TEXT).unwrap();
    let mut branch = main.fork().unwrap();
    assert_eq!(branch.text(), MIXED_TEXT, "{}", R::NAME);

    branch.insert(0, "提案: ").unwrap();
    let pos = byte_pos(&branch.text(), "😀");
    branch.delete(pos, "😀".len()).unwrap();
    let len = main.text().len();
    main.insert(len, "本文の追記").unwrap();
    // マージの前は、本文に分岐の編集が入っていない。
    assert_eq!(
        main.text(),
        format!("{MIXED_TEXT}本文の追記"),
        "{}",
        R::NAME
    );

    main.merge(&mut branch).unwrap();
    let merged = main.text();
    assert!(
        merged.starts_with("提案: 漢字とかな👨"),
        "{}: {merged:?}",
        R::NAME
    );
    assert!(merged.ends_with("本文の追記"), "{}: {merged:?}", R::NAME);
    assert!(!merged.contains('😀'), "{}: {merged:?}", R::NAME);

    // 逆向きにマージすると、分岐も同じ本文になる。
    branch.merge(&mut main).unwrap();
    assert_eq!(branch.text(), merged, "{}", R::NAME);
}

fn マージしない分岐を捨てても本文は変わらない<R: Replica>() {
    let mut main = R::new();
    main.insert(0, "本文").unwrap();
    {
        let mut branch = main.fork().unwrap();
        branch.insert(0, "捨てる提案").unwrap();
    }
    assert_eq!(main.text(), "本文", "{}", R::NAME);
    main.insert(0, "続き").unwrap();
    assert_eq!(main.text(), "続き本文", "{}", R::NAME);
}

fn 届いた更新の変更の列を変更の前の本文に当てると変更のあとの本文になる<
    R: Replica,
>() {
    let mut rs = replicas::<R>(2, MIXED_TEXT);
    let mut rng = SeededRng::new(5);
    const FRAGMENTS: &[&str] = &["あ", "漢字", "😀", "👨\u{200D}👩\u{200D}👧", "\r\n", "a"];

    for step in 0..200 {
        // 相手の側で、挿入か削除を1件から3件まとめて行う。
        let mut pending = Vec::new();
        for _ in 0..=rng.below(3) {
            let text = rs[1].text();
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            pending.push(if boundaries.len() == 1 || rng.below(2) == 0 {
                rs[1]
                    .insert(*rng.pick(&boundaries), rng.pick(FRAGMENTS))
                    .unwrap()
            } else {
                let start = rng.below(boundaries.len() - 1);
                let end = (start + 1 + rng.below(4)).min(boundaries.len() - 1);
                rs[1]
                    .delete(boundaries[start], boundaries[end] - boundaries[start])
                    .unwrap()
            });
        }
        // 手元の側でも編集し、手元の編集が変更の列に混ざらないことを確かめる。
        if rng.below(3) == 0 {
            let len = rs[0].text().len();
            let local = rs[0].insert(len, "手").unwrap();
            rs[1].apply_update(&local).unwrap();
        }
        for update in pending {
            let before = rs[0].text();
            let changes = rs[0].apply_update_with_changes(&update).unwrap();
            assert_eq!(
                TextChange::apply_all(&before, &changes),
                rs[0].text(),
                "{}: 手{step}の変更の列 {changes:?}",
                R::NAME
            );
        }
    }
}

fn 前の更新が届いていない更新の変更の列は空になり届いたときに出る<
    R: Replica,
>() {
    let mut rs = replicas::<R>(2, "あいう");
    let first = rs[1].insert(0, "前").unwrap();
    let second = rs[1].insert(0, "後").unwrap();

    let before = rs[0].text();
    let changes = rs[0].apply_update_with_changes(&second).unwrap();
    assert_eq!(
        TextChange::apply_all(&before, &changes),
        rs[0].text(),
        "{}",
        R::NAME
    );
    assert_eq!(
        rs[0].text(),
        "あいう",
        "{}: 保留した更新が当たっている",
        R::NAME
    );

    let changes = rs[0].apply_update_with_changes(&first).unwrap();
    assert_eq!(
        TextChange::apply_all("あいう", &changes),
        "後前あいう",
        "{}",
        R::NAME
    );
    assert_eq!(rs[0].text(), "後前あいう", "{}", R::NAME);
}

fn 履歴をまとめたスナップショットから今の本文を読み込める<R: Replica>() {
    let mut author = R::new();
    author.insert(0, MIXED_TEXT).unwrap();
    let len = author.text().len();
    author.delete(0, len).unwrap();
    author.insert(0, "残った本文").unwrap();

    match author.compacted_snapshot() {
        Some(compacted) => {
            let mut loaded = R::from_snapshot(&compacted).unwrap();
            assert_eq!(loaded.text(), "残った本文", "{}", R::NAME);
            // まとめたあとも編集を続けられ、元の文書とマージできる。
            let update = loaded.insert(0, "続き").unwrap();
            author.apply_update(&update).unwrap();
            assert_eq!(author.text(), "続き残った本文", "{}", R::NAME);
        }
        None => assert_eq!(R::NAME, "automerge", "{}: まとめる入口が無い", R::NAME),
    }
}

macro_rules! candidate_tests {
    ($($test:ident),* $(,)?) => {
        mod yrs {
            $(#[test] fn $test() { super::$test::<crdt_comparison::YrsReplica>() })*
        }
        mod automerge {
            $(#[test] fn $test() { super::$test::<crdt_comparison::AutomergeReplica>() })*
        }
        mod loro {
            $(#[test] fn $test() { super::$test::<crdt_comparison::LoroReplica>() })*
        }
    };
}

candidate_tests!(
    編集の途中で取った版の本文を別の複製で取り出せる,
    同時の編集をマージしたあとの版の本文を取り出せる,
    分岐を別々に編集してマージすると両方の編集が入る,
    マージしない分岐を捨てても本文は変わらない,
    届いた更新の変更の列を変更の前の本文に当てると変更のあとの本文になる,
    前の更新が届いていない更新の変更の列は空になり届いたときに出る,
    履歴をまとめたスナップショットから今の本文を読み込める,
);
