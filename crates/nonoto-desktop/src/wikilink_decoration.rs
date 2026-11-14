//! 本文の入力で、Wikiリンクを装飾する。
//!
//! Wikiリンクの範囲はコアの解析 (`nonoto_core::find_wikilinks`) で求め、ここでは範囲を装飾に変える。
//! 一般のMarkdownの記法の装飾 (`markdown_highlight`) より優先して描かれる。

use gpui_kit::HighlightStyle;
use gpui_kit::component::input::TextDecoration;
use nonoto_core::Wikilink;

/// Wikiリンクを装飾に変える。
///
/// `[[` と `]]` は記法の記号の見た目 (`mark`)、間のタイトルはリンクの見た目 (`link`) にする。
/// 重なった装飾はどちらが勝つかが決まらないため、3つの範囲を重ならないように分ける。
pub fn decorations(
    links: &[Wikilink],
    link: HighlightStyle,
    mark: HighlightStyle,
) -> Vec<TextDecoration> {
    links
        .iter()
        .flat_map(|wikilink| {
            let opening = wikilink.opening_range();
            let closing = wikilink.closing_range();
            [
                TextDecoration::new(opening.clone(), mark),
                TextDecoration::new(opening.end..closing.start, link),
                TextDecoration::new(closing, mark),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{FontWeight, red};

    #[test]
    fn 記号とタイトルを分けて装飾する() {
        let text = "前に[[はじめに]]と[[次]]";
        let links = nonoto_core::find_wikilinks(text);
        let link = HighlightStyle {
            color: Some(red()),
            ..Default::default()
        };
        let mark = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };

        let decorations = decorations(&links, link, mark);
        let actual: Vec<(&str, HighlightStyle)> = decorations
            .iter()
            .map(|decoration| (&text[decoration.range.clone()], decoration.style))
            .collect();
        assert_eq!(
            actual,
            [
                ("[[", mark),
                ("はじめに", link),
                ("]]", mark),
                ("[[", mark),
                ("次", link),
                ("]]", mark),
            ],
            "装飾の範囲か見た目が違う"
        );
    }
}
