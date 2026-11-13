use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use uuid::Uuid;

/// ページのID。
///
/// 作成した時刻の順に並ぶUUID (v7) を使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageId(Uuid);

impl PageId {
    /// 新しいIDを作る。
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for PageId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for PageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for PageId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

/// ページ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub id: PageId,
    pub title: String,
    /// Markdownの本文。
    pub body: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 一覧に並べるページ。本文を含めない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSummary {
    pub id: PageId,
    pub title: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 作成するページの内容。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewPage {
    pub title: String,
    pub body: String,
}

/// ページの更新。`None` の項目は今の値のまま残す。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageUpdate {
    pub title: Option<String>,
    pub body: Option<String>,
}
