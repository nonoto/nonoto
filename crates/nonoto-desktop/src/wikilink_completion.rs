//! 本文の入力で、Wikiリンクのタイトルを補完する。
//!
//! 入力中のWikiリンクの判定と、候補を選んだときの置き換えはコア (`nonoto_core`) で行い、
//! ここでは部品の補完の仕組みに結び付ける。

use std::rc::Rc;

use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt, highlight_contains};
use gpui_kit::{App, Result, Task, Window};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionResponse, CompletionTextEdit, Range, TextEdit,
};
use nonoto_core::{complete_wikilink, wikilink_query};

/// 候補に出すページの上限。今のWikinoと同じ件数にする。
pub const CANDIDATE_LIMIT: usize = 10;

/// キーワードからページのタイトルを探す関数。
pub type SearchTitles = dyn Fn(&str, &mut App) -> Vec<String>;

/// Wikiリンクのタイトルの補完のプロバイダー。
pub struct WikilinkCompletion {
    search: Rc<SearchTitles>,
}

impl WikilinkCompletion {
    pub fn new(search: Rc<SearchTitles>) -> Self {
        Self { search }
    }
}

impl CompletionProvider for WikilinkCompletion {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        // Wikiリンクは行をまたがないため、本文の全体ではなくカーソルの行だけを渡す。
        let row = text.offset_to_point(offset).row;
        let line_start = text.line_start_offset(row);
        let line = text.slice_line(row).to_string();
        let cursor = offset - line_start;
        let Some(query) = wikilink_query(&line, cursor) else {
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        };
        let items = (self.search)(&query.keyword, cx)
            .into_iter()
            .filter_map(|title| {
                let edit = complete_wikilink(&line, &query, cursor, &title)?;
                let range = Range::new(
                    text.offset_to_position(line_start + edit.range.start),
                    text.offset_to_position(line_start + edit.range.end),
                );
                Some(CompletionItem {
                    label: title,
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(range, edit.text))),
                    // 強調 (`highlight`) で、探したキーワードの語を使う。
                    data: Some(serde_json::Value::String(query.keyword.clone())),
                    ..Default::default()
                })
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    /// 打った文字だけでは入力中のWikiリンクかを決められないため、打つたびに候補を探し直す。
    /// 入力中でなければ `completions` が空の候補を返し、候補は閉じる。
    fn is_completion_trigger(&self, _offset: usize, _new_text: &str, _cx: &mut App) -> bool {
        true
    }
}

/// 候補のタイトルの中で、候補を探したキーワードの語に当たるところを強調する。
///
/// キーワードは、`completions` が候補の `data` に持たせたものを使う。
/// 部品が渡す `query` は、候補を開いた位置からカーソルまでに打った文字列で、
/// 書きかけのWikiリンクに打ち足したときは探したキーワードの一部にしかならないため使わない。
pub fn highlight(_query: &str, item: &CompletionItem) -> Vec<std::ops::Range<usize>> {
    let Some(keyword) = item.data.as_ref().and_then(serde_json::Value::as_str) else {
        return Vec::new();
    };
    let mut ranges: Vec<std::ops::Range<usize>> = keyword
        .split_whitespace()
        .flat_map(|word| highlight_contains(word, item))
        .collect();
    ranges.sort_by_key(|range| range.start);
    ranges
}
