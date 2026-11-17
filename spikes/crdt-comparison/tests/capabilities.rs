//! 取り消し・位置を保つ仕組み・履歴をまとめたあとの版・マージの前の差分を、候補ごとに動かして確かめる。
//!
//! 入口の形が候補ごとに違うため、共通のトレイトを通さず、ライブラリを直接呼ぶ。

use crdt_comparison::{AutomergeReplica, LoroReplica, Replica, YrsReplica};

mod yrs_capabilities {
    use yrs::types::text::YChange;
    use yrs::undo::Options as UndoOptions;
    use yrs::updates::decoder::Decode;
    use yrs::{
        Assoc, Doc, GetString, IndexedSequence, OffsetKind, Options, ReadTxn, Text, Transact,
        UndoManager, Update,
    };

    use super::*;

    fn doc() -> Doc {
        Doc::with_options(Options {
            offset_kind: OffsetKind::Bytes,
            skip_gc: true,
            ..Options::default()
        })
    }

    fn sync(from: &Doc, to: &Doc, origin: Option<&str>) {
        let update = from
            .transact()
            .encode_state_as_update_v1(&to.transact().state_vector());
        let update = Update::decode_v1(&update).unwrap();
        match origin {
            Some(origin) => to.transact_mut_with(origin).apply_update(update).unwrap(),
            None => to.transact_mut().apply_update(update).unwrap(),
        }
    }

    #[test]
    fn 相手の更新に由来を付けて当てると自分の編集だけを取り消せる() {
        let mine = doc();
        let theirs = doc();
        let text = mine.get_or_insert_text("body");
        let their_text = theirs.get_or_insert_text("body");
        let mut undo: UndoManager = UndoManager::with_options(UndoOptions {
            capture_timeout_millis: 0,
            ..UndoOptions::default()
        });
        undo.expand_scope(&mine, &text);

        text.insert(&mut mine.transact_mut(), 0, "自分の");
        sync(&mine, &theirs, None);
        their_text.insert(&mut theirs.transact_mut(), 0, "相手の");
        sync(&theirs, &mine, Some("remote"));
        assert_eq!(text.get_string(&mine.transact()), "相手の自分の");

        assert!(undo.undo_blocking());
        assert_eq!(text.get_string(&mine.transact()), "相手の");
        assert!(undo.redo_blocking());
        assert_eq!(text.get_string(&mine.transact()), "相手の自分の");
    }

    #[test]
    fn 相手の更新を由来なしで当てると取り消しに相手の編集も入る() {
        let mine = doc();
        let theirs = doc();
        let text = mine.get_or_insert_text("body");
        let their_text = theirs.get_or_insert_text("body");
        let mut undo: UndoManager = UndoManager::with_options(UndoOptions {
            capture_timeout_millis: 0,
            ..UndoOptions::default()
        });
        undo.expand_scope(&mine, &text);

        text.insert(&mut mine.transact_mut(), 0, "自分の");
        sync(&mine, &theirs, None);
        their_text.insert(&mut theirs.transact_mut(), 0, "相手の");
        sync(&theirs, &mine, None);

        assert!(undo.undo_blocking());
        assert_eq!(text.get_string(&mine.transact()), "自分の");
    }

    #[test]
    fn 位置はバイト数で数えられ相手の挿入を越えて保たれる() {
        let mine = doc();
        let theirs = doc();
        let text = mine.get_or_insert_text("body");
        text.insert(&mut mine.transact_mut(), 0, "あいう");
        sync(&mine, &theirs, None);
        let sticky = text
            .sticky_index(&mine.transact(), "あい".len() as u32, Assoc::After)
            .unwrap();
        // 位置を指す値は、バイト列にして送れる。
        let encoded = yrs::updates::encoder::Encode::encode_v1(&sticky);
        let sticky = yrs::StickyIndex::decode_v1(&encoded).unwrap();

        let their_text = theirs.get_or_insert_text("body");
        their_text.insert(&mut theirs.transact_mut(), 0, "😀");
        sync(&theirs, &mine, Some("remote"));
        let offset = sticky.get_offset(&mine.transact()).unwrap();
        assert_eq!(offset.index as usize, "😀あい".len());
    }

    #[test]
    fn 削除した文字を消した文書で古い版を指すとエラーにならず今の本文が返る() {
        let mut author = YrsReplica::new();
        author.insert(0, "消す本文").unwrap();
        let old = author.version();
        author.delete(0, "消す".len()).unwrap();
        let mut compacted =
            YrsReplica::from_snapshot(&author.compacted_snapshot().unwrap()).unwrap();
        assert_eq!(compacted.text(), "本文");
        // 削除した文字が消えているため、版の時点の本文を組み立て直せず、黙って今の本文になる。
        assert_eq!(compacted.text_at(&old).unwrap(), "本文");
    }

    #[test]
    fn 二つの版の間の差分を取り出せる() {
        let doc = doc();
        let text = doc.get_or_insert_text("body");
        text.insert(&mut doc.transact_mut(), 0, "本文");
        let base = doc.transact().snapshot();
        text.insert(&mut doc.transact_mut(), 0, "提案");
        let head = doc.transact().snapshot();
        let diff = text.diff_range(
            &mut doc.transact_mut(),
            Some(&head),
            Some(&base),
            YChange::identity,
        );
        let added: Vec<_> = diff
            .iter()
            .filter(|d| {
                d.ychange
                    .as_ref()
                    .is_some_and(|c| c.kind == yrs::types::text::ChangeKind::Added)
            })
            .map(|d| d.insert.clone().to_string(&doc.transact()))
            .collect();
        assert_eq!(added, vec!["提案".to_owned()]);
    }
}

mod automerge_capabilities {
    use automerge::{
        AutoCommit, ObjType, PatchAction, ROOT, ReadDoc, TextEncoding, transaction::Transactable,
    };

    use super::*;

    fn doc() -> (AutoCommit, automerge::ObjId) {
        let mut doc = AutoCommit::new_with_encoding(TextEncoding::Utf8CodeUnit);
        let text = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
        (doc, text)
    }

    #[test]
    fn 位置はバイト数で数えられ相手の挿入を越えて保たれる() {
        let (mut mine, text) = doc();
        mine.splice_text(&text, 0, 0, "あいう").unwrap();
        let mut theirs = mine.fork();
        let cursor = mine.get_cursor(&text, "あい".len(), None).unwrap();
        // 位置を指す値は、バイト列にして送れる。
        let cursor = automerge::Cursor::try_from(cursor.to_bytes().as_slice()).unwrap();

        theirs.splice_text(&text, 0, 0, "😀").unwrap();
        mine.merge(&mut theirs).unwrap();
        assert_eq!(
            mine.get_cursor_position(&text, &cursor, None).unwrap(),
            "😀あい".len()
        );
    }

    #[test]
    fn マージの前に分岐の差分をパッチで取り出せる() {
        let (mut main, text) = doc();
        main.splice_text(&text, 0, 0, "本文").unwrap();
        let mut branch = main.fork();
        let base = branch.get_heads();
        branch.splice_text(&text, 0, 0, "提案").unwrap();
        let head = branch.get_heads();
        let patches = branch.diff(&base, &head);
        assert!(patches.iter().any(|p| matches!(
            &p.action,
            PatchAction::SpliceText { index: 0, value, .. } if value.make_string() == "提案"
        )));
        assert_eq!(main.text(&text).unwrap(), "本文");
    }

    #[test]
    fn 履歴をまとめる入口が無い() {
        assert!(AutomergeReplica::new().compacted_snapshot().is_none());
    }
}

mod loro_capabilities {
    use loro::cursor::Side;
    use loro::{ExportMode, LoroDoc, UndoManager};

    use super::*;

    #[test]
    fn 取り消しは既定で自分の編集だけを戻す() {
        let mine = LoroDoc::new();
        let theirs = LoroDoc::new();
        let mut undo = UndoManager::new(&mine);
        let text = mine.get_text("body");

        text.insert(0, "自分の").unwrap();
        mine.commit();
        theirs
            .import(&mine.export(ExportMode::Snapshot).unwrap())
            .unwrap();
        theirs.get_text("body").insert(0, "相手の").unwrap();
        theirs.commit();
        mine.import(
            &theirs
                .export(ExportMode::updates(&mine.oplog_vv()))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(text.to_string(), "相手の自分の");

        assert!(undo.undo().unwrap());
        assert_eq!(text.to_string(), "相手の");
        assert!(undo.redo().unwrap());
        assert_eq!(text.to_string(), "相手の自分の");
    }

    #[test]
    fn 位置はコードポイントで数えられ相手の挿入を越えて保たれる() {
        let mine = LoroDoc::new();
        let text = mine.get_text("body");
        text.insert(0, "あいう").unwrap();
        mine.commit();
        let theirs = mine.fork();
        // 位置の指定と結果は、コードポイントで数える。
        let cursor = text.get_cursor(2, Side::Middle).unwrap();
        // 位置を指す値は、バイト列にして送れる。
        let cursor = loro::cursor::Cursor::decode(&cursor.encode()).unwrap();

        theirs.get_text("body").insert(0, "😀").unwrap();
        theirs.commit();
        mine.import(
            &theirs
                .export(ExportMode::updates(&mine.oplog_vv()))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(mine.get_cursor_pos(&cursor).unwrap().current.pos, 3);
    }

    #[test]
    fn 履歴をまとめた文書では古い版の本文を取り出せない() {
        let mut author = LoroReplica::new();
        author.insert(0, "消す本文").unwrap();
        let old = author.version();
        author.delete(0, "消す".len()).unwrap();
        let mut compacted =
            LoroReplica::from_snapshot(&author.compacted_snapshot().unwrap()).unwrap();
        assert_eq!(compacted.text(), "本文");
        assert!(compacted.text_at(&old).is_err());
    }

    #[test]
    fn マージの前に分岐の差分を取り出せる() {
        let main = LoroDoc::new();
        main.get_text("body").insert(0, "本文").unwrap();
        main.commit();
        let branch = main.fork();
        let base = branch.oplog_frontiers();
        branch.get_text("body").insert(0, "提案").unwrap();
        branch.commit();
        let diff = branch.diff(&base, &branch.oplog_frontiers()).unwrap();
        assert!(format!("{diff:?}").contains("提案"));
        assert_eq!(main.get_text("body").to_string(), "本文");
    }
}
