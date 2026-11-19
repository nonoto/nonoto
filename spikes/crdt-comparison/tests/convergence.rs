//! 同時の編集を、どの順で当てても同じ本文に収束するかを、すべての候補で確かめる。

mod common;

use std::collections::BTreeSet;

use common::{SeededRng, assert_converged, broadcast, byte_pos, replicas};
use crdt_comparison::{AutomergeReplica, LoroReplica, Replica, YrsReplica};

/// 漢字・かな・絵文字 (サロゲートペアになる文字・ZWJでつないだ絵文字・国旗)・結合文字・CRLFを含む本文。
const MIXED_TEXT: &str = "漢字とかな😀👨\u{200D}👩\u{200D}👧🇯🇵\r\nか\u{3099}が\r\n";

fn 同じ位置への同時の挿入が2つの複製で収束する<R: Replica>() {
    let mut rs = replicas::<R>(2, "あいう");
    let pos = byte_pos(&rs[0].text(), "い");
    let a = rs[0]
        .insert(pos, "甲")
        .expect("文書に文字列を挿入できなかった");
    let b = rs[1]
        .insert(pos, "乙")
        .expect("文書に文字列を挿入できなかった");
    broadcast(&mut rs, 0, &a);
    broadcast(&mut rs, 1, &b);

    let text = assert_converged(&rs);
    assert!(
        text == "あ甲乙いう" || text == "あ乙甲いう",
        "{}: 同時に挿入した2つの文字列が、挿入した位置に並んでいない",
        R::NAME
    );
}

fn 同じ位置への同時の挿入が3つの複製で届く順によらず収束する<
    R: Replica,
>() {
    let mut rs = replicas::<R>(3, "あいう");
    let pos = byte_pos(&rs[0].text(), "い");
    let updates = [
        rs[0]
            .insert(pos, "甲")
            .expect("文書に文字列を挿入できなかった"),
        rs[1]
            .insert(pos, "乙")
            .expect("文書に文字列を挿入できなかった"),
        rs[2]
            .insert(pos, "丙")
            .expect("文書に文字列を挿入できなかった"),
    ];
    // 複製ごとに、ほかの2つの更新を別の順で受け取る。
    for (to, order) in [(0, [2, 1]), (1, [0, 2]), (2, [1, 0])] {
        for from in order {
            rs[to]
                .apply_update(&updates[from])
                .expect("更新を当てられなかった");
        }
    }

    let text = assert_converged(&rs);
    let middle = text
        .strip_prefix("あ")
        .and_then(|rest| rest.strip_suffix("いう"))
        .unwrap_or_else(|| panic!("{}: 挿入の前後の本文が崩れている", R::NAME));
    let mut chars: Vec<char> = middle.chars().collect();
    chars.sort_unstable();
    let mut expected: Vec<char> = "甲乙丙".chars().collect();
    expected.sort_unstable();
    assert_eq!(
        chars,
        expected,
        "{}: 3つの挿入が過不足なく並んでいない",
        R::NAME
    );
}

fn 重なる範囲の同時の削除が収束する<R: Replica>() {
    let mut rs = replicas::<R>(2, "あいうえお");
    let text = rs[0].text();
    let a = rs[0]
        .delete(byte_pos(&text, "い"), "いう".len())
        .expect("本文を削除できなかった");
    let b = rs[1]
        .delete(byte_pos(&text, "う"), "うえ".len())
        .expect("本文を削除できなかった");
    broadcast(&mut rs, 0, &a);
    broadcast(&mut rs, 1, &b);

    assert_eq!(assert_converged(&rs), "あお", "{}", R::NAME);
}

fn 片方が削除した範囲の中への挿入が収束する<R: Replica>() {
    let mut rs = replicas::<R>(2, "あいうえお");
    let text = rs[0].text();
    let a = rs[0]
        .delete(byte_pos(&text, "い"), "いうえ".len())
        .expect("本文を削除できなかった");
    let b = rs[1]
        .insert(byte_pos(&text, "う"), "差")
        .expect("文書に文字列を挿入できなかった");
    broadcast(&mut rs, 0, &a);
    broadcast(&mut rs, 1, &b);

    // 削除された範囲の中への挿入は、どの候補でも残る。
    assert_eq!(assert_converged(&rs), "あ差お", "{}", R::NAME);
}

fn 三つの複製での同時の挿入と削除が収束する<R: Replica>() {
    let mut rs = replicas::<R>(3, "一二三四五六七八九");
    let text = rs[0].text();
    let updates = [
        (
            0,
            rs[0]
                .delete(byte_pos(&text, "二"), "二三四".len())
                .expect("本文を削除できなかった"),
        ),
        (
            1,
            rs[1]
                .delete(byte_pos(&text, "四"), "四五六".len())
                .expect("本文を削除できなかった"),
        ),
        (
            2,
            rs[2]
                .insert(byte_pos(&text, "三"), "甲")
                .expect("文書に文字列を挿入できなかった"),
        ),
        (
            2,
            rs[2]
                .insert(byte_pos(&text, "八") + "甲".len(), "乙")
                .expect("文書に文字列を挿入できなかった"),
        ),
    ];
    for (from, update) in &updates {
        broadcast(&mut rs, *from, update);
    }

    assert_eq!(assert_converged(&rs), "一甲七乙八九", "{}", R::NAME);
}

fn 多様な文字を含む本文での同時の編集が収束する<R: Replica>() {
    let mut rs = replicas::<R>(2, MIXED_TEXT);
    let mut updates = Vec::new();

    // 複製0: ZWJでつないだ絵文字を消し、サロゲートペアの絵文字の前と、CRLFの直後に挿入する。
    let text = rs[0].text();
    let family = "👨\u{200D}👩\u{200D}👧";
    updates.push((
        0,
        rs[0]
            .delete(byte_pos(&text, family), family.len())
            .expect("本文を削除できなかった"),
    ));
    let text = rs[0].text();
    updates.push((
        0,
        rs[0]
            .insert(byte_pos(&text, "😀"), "🎉")
            .expect("文書に文字列を挿入できなかった"),
    ));
    let text = rs[0].text();
    updates.push((
        0,
        rs[0]
            .insert(byte_pos(&text, "\r\n") + "\r\n".len(), "行")
            .expect("文書に文字列を挿入できなかった"),
    ));

    // 複製1: 結合文字 (濁点) だけを消し、国旗の前と末尾に挿入する。
    let text = rs[1].text();
    updates.push((
        1,
        rs[1]
            .delete(byte_pos(&text, "\u{3099}"), "\u{3099}".len())
            .expect("本文を削除できなかった"),
    ));
    let text = rs[1].text();
    updates.push((
        1,
        rs[1]
            .insert(byte_pos(&text, "🇯🇵"), "🇺🇸")
            .expect("文書に文字列を挿入できなかった"),
    ));
    let len = rs[1].text().len();
    updates.push((
        1,
        rs[1]
            .insert(len, "末尾")
            .expect("文書に文字列を挿入できなかった"),
    ));

    for (from, update) in &updates {
        broadcast(&mut rs, *from, update);
    }

    assert_eq!(
        assert_converged(&rs),
        "漢字とかな🎉😀🇺🇸🇯🇵\r\n行かが\r\n末尾",
        "{}",
        R::NAME
    );
}

fn delivery_orders() -> Vec<[usize; 4]> {
    fn permute(order: &mut [usize; 4], at: usize, orders: &mut Vec<[usize; 4]>) {
        if at == order.len() {
            orders.push(*order);
            return;
        }
        for next in at..order.len() {
            order.swap(at, next);
            permute(order, at + 1, orders);
            order.swap(at, next);
        }
    }

    let mut orders = Vec::new();
    permute(&mut [0, 1, 2, 3], 0, &mut orders);
    orders
}

fn 依存する更新を順不同と重複で当てても収束する<R: Replica>() {
    let mut author = R::new();
    let start = author.snapshot();
    let updates = [
        author
            .insert(0, "あいう")
            .expect("文書に文字列を挿入できなかった"),
        author
            .insert("あいう".len(), "😀\r\n")
            .expect("文書に文字列を挿入できなかった"),
        author
            .delete(0, "あ".len())
            .expect("本文を削除できなかった"),
        author
            .insert(0, "か\u{3099}")
            .expect("文書に文字列を挿入できなかった"),
    ];
    let expected = author.text();
    assert_eq!(expected, "か\u{3099}いう😀\r\n", "{}", R::NAME);

    let orders = delivery_orders();
    assert_eq!(orders.len(), 24, "配送順が24通りでない");
    assert_eq!(
        orders.iter().collect::<BTreeSet<_>>().len(),
        24,
        "配送順に重複がある"
    );
    assert!(orders.contains(&[3, 2, 1, 0]), "完全な逆順が含まれない");
    for order in orders {
        let mut reader = R::from_snapshot(&start).expect("スナップショットから文書を作れなかった");
        for &i in &order {
            reader
                .apply_update(&updates[i])
                .expect("更新を当てられなかった");
            // 同じ更新を続けて2回当てる。
            reader
                .apply_update(&updates[i])
                .expect("更新を当てられなかった");
        }
        assert_eq!(reader.text(), expected, "{}: 当てた順 {order:?}", R::NAME);

        // 揃ったあとに、すべての更新をもう一度当てる。
        for update in &updates {
            reader.apply_update(update).expect("更新を当てられなかった");
        }
        assert_eq!(
            reader.text(),
            expected,
            "{}: 2回目の適用 {order:?}",
            R::NAME
        );
    }
}

fn スナップショットから作った複製で編集を続けても収束する<R: Replica>() {
    let mut rs = replicas::<R>(2, MIXED_TEXT);
    let a = rs[0]
        .insert(0, "前")
        .expect("文書に文字列を挿入できなかった");
    broadcast(&mut rs, 0, &a);

    let late = rs[1].replicate().expect("文書を写せなかった");
    assert_eq!(late.text(), rs[0].text(), "{}", R::NAME);
    rs.push(late);
    let len = rs[2].text().len();
    let b = rs[2]
        .insert(len, "後")
        .expect("文書に文字列を挿入できなかった");
    let c = rs[0]
        .insert(0, "頭")
        .expect("文書に文字列を挿入できなかった");
    broadcast(&mut rs, 2, &b);
    broadcast(&mut rs, 0, &c);

    let text = assert_converged(&rs);
    assert!(
        text.starts_with("頭前漢字") && text.ends_with("\r\n後"),
        "{}",
        R::NAME
    );
}

fn 書き手を指定した複製が既存の履歴から編集を続けられる<R: Replica>() {
    let mut first = R::with_writer_id(1);
    first
        .insert(0, MIXED_TEXT)
        .expect("文書に文字列を挿入できなかった");
    let mut other = R::from_snapshot_with_writer_id(&first.snapshot(), 2)
        .expect("スナップショットから文書を作れなかった");
    assert_eq!(other.text(), MIXED_TEXT, "{}", R::NAME);
    let a = first
        .insert(0, "前")
        .expect("文書に文字列を挿入できなかった");
    let b = other
        .insert(MIXED_TEXT.len(), "後")
        .expect("文書に文字列を挿入できなかった");
    first.apply_update(&b).expect("更新を当てられなかった");
    other.apply_update(&a).expect("更新を当てられなかった");
    assert_eq!(first.text(), format!("前{MIXED_TEXT}後"), "{}", R::NAME);
    assert_eq!(first.text(), other.text(), "{}", R::NAME);
}

#[derive(Debug, PartialEq, Eq)]
enum SessionEvent {
    Insert {
        actor: usize,
        pos: usize,
        text: &'static str,
    },
    Delete {
        actor: usize,
        pos: usize,
        len: usize,
    },
    Deliver {
        to: usize,
        update: usize,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct SessionTrace {
    writers: [u32; 3],
    events: Vec<SessionEvent>,
    text: String,
}

#[derive(Clone, Copy)]
enum DeliveryMode {
    DuringEdits,
    AfterEdits,
}

fn deliver<R: Replica>(
    replicas: &mut [R],
    updates: &[Vec<u8>],
    to: usize,
    update: usize,
    events: &mut Vec<SessionEvent>,
) {
    replicas[to]
        .apply_update(&updates[update])
        .expect("更新を当てられなかった");
    events.push(SessionEvent::Deliver { to, update });
}

/// 書き手・操作・配送を種から決め、3つの複製に当てた列と収束した本文を返す。
fn run_random_session<R: Replica>(
    seed: u64,
    steps: usize,
    delivery_mode: DeliveryMode,
) -> SessionTrace {
    const FRAGMENTS: &[&str] = &[
        "あ",
        "漢字",
        "です。",
        "😀",
        "👨\u{200D}👩\u{200D}👧",
        "🇯🇵",
        "か\u{3099}",
        "\r\n",
        "a",
        "# 見出し\r\n",
    ];

    let mut rng = SeededRng::new(seed);
    // 下位2ビットを複製番号に割り当て、3つの書き手IDを重ねない。
    let base = u32::try_from(rng.next_u64() & u64::from(u32::MAX))
        .expect("下位32ビットはu32に収まる")
        & !3;
    let writers = [base, base | 1, base | 2];
    let mut first = R::with_writer_id(writers[0]);
    let snapshot = first.snapshot();
    let mut rs = vec![first];
    for &writer in &writers[1..] {
        rs.push(
            R::from_snapshot_with_writer_id(&snapshot, writer)
                .expect("スナップショットから文書を作れなかった"),
        );
    }
    let mut events = Vec::new();
    let mut updates = Vec::new();
    // 複製ごとの、まだ届いていない更新と、届いた更新。
    let mut inbox: Vec<Vec<usize>> = vec![Vec::new(); rs.len()];
    let mut delivered: Vec<Vec<usize>> = vec![Vec::new(); rs.len()];

    for _ in 0..steps {
        let actor = rng.below(rs.len());
        let text = rs[actor].text();
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        let update = if boundaries.len() == 1 || rng.below(10) < 6 {
            let pos = *rng.pick(&boundaries);
            let text = *rng.pick(FRAGMENTS);
            events.push(SessionEvent::Insert { actor, pos, text });
            rs[actor]
                .insert(pos, text)
                .expect("文書に文字列を挿入できなかった")
        } else {
            let start = rng.below(boundaries.len() - 1);
            let end = (start + 1 + rng.below(5)).min(boundaries.len() - 1);
            let pos = boundaries[start];
            let len = boundaries[end] - pos;
            events.push(SessionEvent::Delete { actor, pos, len });
            rs[actor].delete(pos, len).expect("本文を削除できなかった")
        };
        let update_id = updates.len();
        updates.push(update);
        for (to, queue) in inbox.iter_mut().enumerate() {
            if to != actor {
                queue.push(update_id);
            }
        }

        // 一部の複製に、溜まった更新の一部を、順を入れ替えて届ける。
        if matches!(delivery_mode, DeliveryMode::DuringEdits) && rng.below(3) == 0 {
            let to = rng.below(rs.len());
            rng.shuffle(&mut inbox[to]);
            let count = rng.below(inbox[to].len() + 1);
            for update in inbox[to].drain(..count).collect::<Vec<_>>() {
                deliver(&mut rs, &updates, to, update, &mut events);
                delivered[to].push(update);
            }
        }
        // 届いた更新を、ときどき重ねて届ける。
        if matches!(delivery_mode, DeliveryMode::DuringEdits) && rng.below(5) == 0 {
            let to = rng.below(rs.len());
            if !delivered[to].is_empty() {
                let again = *rng.pick(&delivered[to]);
                deliver(&mut rs, &updates, to, again, &mut events);
            }
        }
    }

    for (to, queue) in inbox.iter_mut().enumerate() {
        rng.shuffle(queue);
        for &update in queue.iter() {
            deliver(&mut rs, &updates, to, update, &mut events);
        }
        for &update in queue.iter() {
            deliver(&mut rs, &updates, to, update, &mut events);
        }
    }

    let text = assert_converged(&rs);
    assert!(!text.is_empty(), "{}: 種{seed}で本文が空になった", R::NAME);
    SessionTrace {
        writers,
        events,
        text,
    }
}

fn 乱数で作った長い操作の列が収束する<R: Replica>() {
    for seed in [7, 42, 2026] {
        let first = run_random_session::<R>(seed, 300, DeliveryMode::DuringEdits);
        let again = run_random_session::<R>(seed, 300, DeliveryMode::DuringEdits);
        assert_eq!(
            first,
            again,
            "{}: 種{seed}の操作と配送を再現できない",
            R::NAME
        );
    }
}

#[test]
fn 長い操作と配送の列がすべての候補で一致する() {
    for seed in [7, 42, 2026] {
        // 編集のあとまで配送を遅らせ、候補ごとの同時挿入の並びに、次の操作を左右させない。
        let yrs = run_random_session::<YrsReplica>(seed, 300, DeliveryMode::AfterEdits);
        let automerge = run_random_session::<AutomergeReplica>(seed, 300, DeliveryMode::AfterEdits);
        let loro = run_random_session::<LoroReplica>(seed, 300, DeliveryMode::AfterEdits);
        assert_eq!(yrs.writers, automerge.writers, "種{seed}の書き手IDが違う");
        assert_eq!(yrs.writers, loro.writers, "種{seed}の書き手IDが違う");
        assert_eq!(yrs.events, automerge.events, "種{seed}の操作と配送が違う");
        assert_eq!(yrs.events, loro.events, "種{seed}の操作と配送が違う");
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
    同じ位置への同時の挿入が2つの複製で収束する,
    同じ位置への同時の挿入が3つの複製で届く順によらず収束する,
    重なる範囲の同時の削除が収束する,
    片方が削除した範囲の中への挿入が収束する,
    三つの複製での同時の挿入と削除が収束する,
    多様な文字を含む本文での同時の編集が収束する,
    依存する更新を順不同と重複で当てても収束する,
    スナップショットから作った複製で編集を続けても収束する,
    書き手を指定した複製が既存の履歴から編集を続けられる,
    乱数で作った長い操作の列が収束する,
);
