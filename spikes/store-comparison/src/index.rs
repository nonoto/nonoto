//! 復号したページから作る、メモリ上の検索のインデックス。
//!
//! 日本語には単語の区切りが無いため、連続する2文字 (バイグラム) ごとにページを引けるようにする。

use std::collections::{HashMap, HashSet};

use nonoto_core::PageId;

/// バイグラムからページを引く転置インデックス。
#[derive(Default)]
pub struct BigramIndex {
    postings: HashMap<(char, char), HashSet<PageId>>,
}

impl BigramIndex {
    /// ページのテキストを登録する。
    pub fn add(&mut self, id: PageId, texts: &[&str]) {
        for bigram in texts.iter().flat_map(|text| bigrams(text)) {
            self.postings.entry(bigram).or_default().insert(id);
        }
    }

    /// ページのテキストの登録を外す。`texts` は登録したときと同じテキストを渡す。
    pub fn remove(&mut self, id: PageId, texts: &[&str]) {
        for bigram in texts.iter().flat_map(|text| bigrams(text)) {
            if let Some(ids) = self.postings.get_mut(&bigram) {
                ids.remove(&id);
                if ids.is_empty() {
                    self.postings.remove(&bigram);
                }
            }
        }
    }

    /// クエリのすべてのバイグラムを含むページの候補を返す。
    ///
    /// バイグラムを作れない1文字のクエリでは `None` を返し、呼び出し元にすべてのページを調べさせる。
    /// 候補はバイグラムの並びまでは確かめないため、呼び出し元が部分一致を確かめる。
    pub fn candidates(&self, query: &str) -> Option<HashSet<PageId>> {
        let mut query_bigrams = bigrams(query).peekable();
        query_bigrams.peek()?;
        let mut result: Option<HashSet<PageId>> = None;
        for bigram in query_bigrams {
            let Some(ids) = self.postings.get(&bigram) else {
                return Some(HashSet::new());
            };
            result = Some(match result {
                None => ids.clone(),
                Some(current) => current.intersection(ids).copied().collect(),
            });
        }
        result
    }
}

fn bigrams(text: &str) -> impl Iterator<Item = (char, char)> + '_ {
    text.chars().zip(text.chars().skip(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn すべてのバイグラムを含むページだけが候補になる() {
        let mut index = BigramIndex::default();
        let tokyo = PageId::new();
        let kyoto = PageId::new();
        index.add(tokyo, &["東京都の会議"]);
        index.add(kyoto, &["京都の会議"]);

        let candidates = index.candidates("東京").expect("候補を作れなかった");

        assert_eq!(candidates, HashSet::from([tokyo]), "候補が期待と異なる");
    }

    #[test]
    fn 外したページは候補にならない() {
        let mut index = BigramIndex::default();
        let id = PageId::new();
        index.add(id, &["議事録"]);

        index.remove(id, &["議事録"]);

        assert_eq!(
            index.candidates("議事"),
            Some(HashSet::new()),
            "外したページが候補に残った"
        );
    }

    #[test]
    fn 一文字のクエリでは候補を絞らない() {
        let index = BigramIndex::default();

        assert_eq!(index.candidates("会"), None, "1文字のクエリで候補を絞った");
    }
}
