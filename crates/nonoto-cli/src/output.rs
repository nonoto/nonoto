//! 標準出力に書くJSONの形。
//!
//! コマンドの出力はエージェントやテストが読むインターフェースなので、コアの型とは別に形を固定する。

use nonoto_core::{Page, PageSummary};
use serde::Serialize;

/// ページ1件の出力。日時はRFC 3339の文字列にする。
#[derive(Serialize)]
pub struct PageOutput {
    id: String,
    title: String,
    body: String,
    created_at: String,
    updated_at: String,
}

impl From<Page> for PageOutput {
    fn from(page: Page) -> Self {
        Self {
            id: page.id.to_string(),
            title: page.title,
            body: page.body,
            created_at: page.created_at.to_string(),
            updated_at: page.updated_at.to_string(),
        }
    }
}

/// 一覧の1件の出力。本文を含めない。
#[derive(Serialize)]
pub struct PageSummaryOutput {
    id: String,
    title: String,
    created_at: String,
    updated_at: String,
}

impl From<PageSummary> for PageSummaryOutput {
    fn from(summary: PageSummary) -> Self {
        Self {
            id: summary.id.to_string(),
            title: summary.title,
            created_at: summary.created_at.to_string(),
            updated_at: summary.updated_at.to_string(),
        }
    }
}
