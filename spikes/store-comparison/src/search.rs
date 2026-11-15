//! 比較用のSQLCipherの検索。方式Bと同じ項目を、同じ順序で返す。

use jiff::Timestamp;
use nonoto_core::{Error, PageSummary};
use rusqlite::{Connection, params};
use store_comparison::tokenizer::Tokenizer;

/// 全文検索のインデックスに使うトークナイザー。
#[derive(Clone, Copy, Debug)]
pub enum FtsIndex {
    /// FTS5の組み込みのtrigram。
    Trigram,
    /// `store_comparison::fts5` で登録するトークナイザー。
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "計測ではまだtrigramだけを計り、テストだけが使う")
    )]
    Custom(Tokenizer),
}

impl FtsIndex {
    fn tokenizer_name(self) -> &'static str {
        match self {
            Self::Trigram => "trigram",
            Self::Custom(tokenizer) => tokenizer.name(),
        }
    }
}

/// 全文検索のインデックスを作り、ページの保存と更新をインデックスに反映するトリガーを付ける。
///
/// コアのページの操作は作成と更新だけなので、挿入と更新のトリガーを付ける。
/// [`FtsIndex::Custom`] では、このあとにページを保存する接続にもトークナイザーを登録しておく
/// (`store_comparison::fts5::register`)。トークナイザーを登録していない接続からは保存できなくなる。
pub fn build_fts(conn: &Connection, index: FtsIndex) -> Result<(), Error> {
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE pages_fts USING fts5(title, body, content='pages', tokenize='{}');
         INSERT INTO pages_fts(pages_fts) VALUES ('rebuild');
         CREATE TRIGGER pages_fts_insert AFTER INSERT ON pages BEGIN
             INSERT INTO pages_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
         END;
         CREATE TRIGGER pages_fts_update AFTER UPDATE ON pages BEGIN
             INSERT INTO pages_fts(pages_fts, rowid, title, body)
                 VALUES ('delete', old.rowid, old.title, old.body);
             INSERT INTO pages_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
         END;",
        index.tokenizer_name()
    ))?;
    Ok(())
}

/// 文字列の部分一致で全件を走査する。大文字小文字やSQLのワイルドカードも文字どおりに扱う。
pub fn scan(conn: &Connection, query: &str) -> Result<Vec<PageSummary>, Error> {
    search(
        conn,
        "SELECT id, title, created_at, updated_at FROM pages
         WHERE instr(title, ?1) > 0 OR instr(body, ?1) > 0
         ORDER BY updated_at DESC, id DESC",
        query,
        "",
    )
}

/// FTS5のインデックスで検索する。結果は [`scan`] と同じになる。
///
/// trigramでは、FTS5で候補を絞ってから部分一致を確かめ、3文字未満は全件を走査する。
/// 自作のトークナイザーでは、クエリをフレーズとして渡し、FTS5の結果だけを返す。
/// フレーズにできない空のクエリは、全件を走査する。
pub fn fts(conn: &Connection, index: FtsIndex, query: &str) -> Result<Vec<PageSummary>, Error> {
    let length = query.chars().count();
    let phrase = format!("\"{}\"", query.replace('"', "\"\""));
    match index {
        FtsIndex::Trigram if length < 3 => scan(conn, query),
        FtsIndex::Trigram => search(
            conn,
            "SELECT p.id, p.title, p.created_at, p.updated_at FROM pages AS p
             JOIN pages_fts ON pages_fts.rowid = p.rowid
             WHERE pages_fts MATCH ?2 AND (instr(p.title, ?1) > 0 OR instr(p.body, ?1) > 0)
             ORDER BY p.updated_at DESC, p.id DESC",
            query,
            &phrase,
        ),
        FtsIndex::Custom(_) if length == 0 => scan(conn, query),
        FtsIndex::Custom(tokenizer) => {
            // バイグラムの1文字のクエリは、その文字で始まるトークンの前方一致で引く。
            let expression = if tokenizer == Tokenizer::Bigram && length == 1 {
                format!("{phrase} *")
            } else {
                phrase
            };
            search(
                conn,
                "SELECT p.id, p.title, p.created_at, p.updated_at FROM pages AS p
                 JOIN pages_fts ON pages_fts.rowid = p.rowid
                 WHERE pages_fts MATCH ?2
                 ORDER BY p.updated_at DESC, p.id DESC",
                query,
                &expression,
            )
        }
    }
}

fn search(
    conn: &Connection,
    sql: &str,
    query: &str,
    quoted: &str,
) -> Result<Vec<PageSummary>, Error> {
    let mut stmt = conn.prepare(sql)?;
    // 走査では?1だけ、FTS5では?1と?2を使う。
    let rows = if quoted.is_empty() {
        stmt.query(params![query])?
    } else {
        stmt.query(params![query, quoted])?
    };
    read_summaries(rows)
}

fn read_summaries(mut rows: rusqlite::Rows<'_>) -> Result<Vec<PageSummary>, Error> {
    let mut summaries = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let timestamp = |column| -> Result<Timestamp, Error> {
            let millis: i64 = row.get(column)?;
            Timestamp::from_millisecond(millis)
                .map_err(|_| Error::CorruptData(format!("日時が範囲外: {millis}")))
        };
        summaries.push(PageSummary {
            id: id
                .parse()
                .map_err(|_| Error::CorruptData(format!("ページのIDがUUIDではない: {id}")))?,
            title: row.get(1)?,
            created_at: timestamp(2)?,
            updated_at: timestamp(3)?,
        });
    }
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use nonoto_core::{Key, Page, PageId, PageStore, SqlCipherStore};
    use store_comparison::record::RecordStore;

    use super::*;

    const INDEXES: [FtsIndex; 3] = [
        FtsIndex::Trigram,
        FtsIndex::Custom(Tokenizer::Bigram),
        FtsIndex::Custom(Tokenizer::Unigram),
    ];

    fn page(title: &str, body: &str, millisecond: i64) -> Page {
        let at = Timestamp::from_millisecond(millisecond).expect("日時を作れなかった");
        Page {
            id: PageId::new(),
            title: title.to_owned(),
            body: body.to_owned(),
            created_at: at,
            updated_at: at,
        }
    }

    fn ids(summaries: Vec<PageSummary>) -> Vec<PageId> {
        summaries.into_iter().map(|summary| summary.id).collect()
    }

    /// コアのストアは鍵を渡したあとの接続を外に出さず、トークナイザーを登録できないため、
    /// 全文検索のインデックスを作ったあとは、コアのストアと同じSQLをこの接続で実行して保存する。
    fn insert(conn: &Connection, page: &Page) {
        conn.execute(
            "INSERT INTO pages (id, title, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                page.id.to_string(),
                page.title,
                page.body,
                page.created_at.as_millisecond(),
                page.updated_at.as_millisecond(),
            ],
        )
        .expect("ページを保存できなかった");
    }

    fn update_body(conn: &Connection, id: PageId, body: &str, millisecond: i64) {
        conn.execute(
            "UPDATE pages SET body = ?2, updated_at = ?3 WHERE id = ?1",
            params![id.to_string(), body, millisecond],
        )
        .expect("ページを更新できなかった");
    }

    #[test]
    fn インデックスを作ったあとの保存と更新を全文検索に反映する() {
        for index in INDEXES {
            let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
            let key = Key::generate().expect("鍵を作れなかった");
            let path = dir.path().join("a.db");
            let existing = page("既存のページ", "古い本文の語句", 1_000);
            SqlCipherStore::open(&path, &key)
                .expect("方式Aを開けなかった")
                .insert(&existing)
                .expect("方式Aに保存できなかった");
            let conn = crate::open_sqlcipher(&path, &key);
            build_fts(&conn, index).expect("全文検索のインデックスを作れなかった");

            let added = page("追加したページ", "追加した本文の語句", 2_000);
            insert(&conn, &added);
            update_body(&conn, existing.id, "新しい本文の語句", 3_000);

            conn.execute(
                "INSERT INTO pages_fts(pages_fts, rank) VALUES ('integrity-check', 1)",
                [],
            )
            .unwrap_or_else(|error| {
                panic!("全文検索のインデックスがページの内容と食い違う: {index:?} {error}")
            });
            assert_eq!(
                ids(fts(&conn, index, "追加した本文").expect("全文検索できなかった")),
                [added.id],
                "保存したページを全文検索で見つけられない: {index:?}"
            );
            assert_eq!(
                ids(fts(&conn, index, "新しい本文").expect("全文検索できなかった")),
                [existing.id],
                "更新した本文を全文検索で見つけられない: {index:?}"
            );
            assert!(
                fts(&conn, index, "古い本文")
                    .expect("全文検索できなかった")
                    .is_empty(),
                "更新する前の本文で見つかった: {index:?}"
            );
        }
    }

    #[test]
    fn 各方式の検索結果の項目と順序が一致する() {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
        let key = Key::generate().expect("鍵を作れなかった");
        let paths: Vec<_> = INDEXES
            .iter()
            .enumerate()
            .map(|(i, _)| dir.path().join(format!("a{i}.db")))
            .collect();
        let mut stores: Vec<_> = paths
            .iter()
            .map(|path| SqlCipherStore::open(path, &key).expect("方式Aを開けなかった"))
            .collect();
        let mut b = RecordStore::open(&dir.path().join("b.db"), &key).expect("方式Bを開けなかった");
        let mut pages = Vec::new();
        for (title, body, millis) in [
            ("会議の議事録", "東京都の議事録の確認", 1_000),
            ("東京都の会議", "本文にABCと100%と_と\"引用\"", 2_000),
            ("別のページ", "会議とabc", 2_000),
            ("スケジュール", "Hello World\nの共有、会", 4_000),
            ("該当しないページ", "大阪", 3_000),
            ("会", "", 5_000),
        ] {
            let page = page(title, body, millis);
            for store in &mut stores {
                store.insert(&page).expect("方式Aに保存できなかった");
            }
            b.insert(&page).expect("方式Bに保存できなかった");
            pages.push(page);
        }
        drop(stores);
        let conns: Vec<_> = INDEXES
            .iter()
            .zip(&paths)
            .map(|(index, path)| {
                let conn = crate::open_sqlcipher(path, &key);
                build_fts(&conn, *index).expect("全文検索のインデックスを作れなかった");
                conn
            })
            .collect();

        for query in [
            "",
            "会",
            "録",
            "大",
            "会議",
            "阪",
            "東京都",
            "議事録の確認",
            "ケジュ",
            "スケジュール",
            "ell",
            "o W",
            "d\nの",
            " ",
            "ABC",
            "abc",
            "%",
            "_",
            "\"引用\"",
            "、会",
            "無い語句",
            "無",
        ] {
            let mut found: Vec<_> = pages
                .iter()
                .filter(|page| page.title.contains(query) || page.body.contains(query))
                .collect();
            found.sort_by_key(|page| std::cmp::Reverse((page.updated_at, page.id.to_string())));
            let expected: Vec<_> = found
                .into_iter()
                .map(|page| PageSummary {
                    id: page.id,
                    title: page.title.clone(),
                    created_at: page.created_at,
                    updated_at: page.updated_at,
                })
                .collect();

            for (index, conn) in INDEXES.iter().zip(&conns) {
                assert_eq!(
                    scan(conn, query).expect("走査で検索できなかった"),
                    expected,
                    "方式Aの走査の結果が異なる: {query:?}"
                );
                assert_eq!(
                    fts(conn, *index, query).expect("全文検索できなかった"),
                    expected,
                    "方式Aの全文検索の結果が異なる: {index:?} {query:?}"
                );
            }
            assert_eq!(
                b.search(query),
                expected,
                "方式Bの検索の結果が異なる: {query:?}"
            );
        }
    }
}
