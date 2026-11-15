//! 本文のWikiリンク (`[[タイトル]]`) の解析。
//!
//! 1つのWikiリンクの範囲は、今のWikinoの本文の描画と同じく、同じ行の `[[` から最初の `]]` までにする。
//! 中身が空白だけのものと、`[[` をバックスラッシュでエスケープしたものと、
//! コード・Markdownのリンクと画像・HTML・参照先の定義の中のものは、Wikiリンクにしない。

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

use crate::editing::TextEdit;

mod html;

/// 本文の中のWikiリンク。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wikilink {
    /// `[[` から `]]` までの範囲。UTF-8のバイト位置で表す。
    pub range: Range<usize>,
    /// リンク先のタイトル。`[[` と `]]` の間から前後の空白を除いたもの。
    pub title: String,
}

const OPENING: &str = "[[";
const CLOSING: &str = "]]";

impl Wikilink {
    /// `[[` の範囲。
    pub fn opening_range(&self) -> Range<usize> {
        self.range.start..self.range.start + OPENING.len()
    }

    /// `]]` の範囲。
    pub fn closing_range(&self) -> Range<usize> {
        self.range.end - CLOSING.len()..self.range.end
    }
}

/// 本文の中のWikiリンクを、位置の順に返す。
///
/// Wikiリンクにしない範囲の外の本文を区切りごとに探すため、その範囲をまたぐ `[[` と `]]` の組はWikiリンクにしない。
pub fn find_wikilinks(text: &str) -> Vec<Wikilink> {
    if !text.contains(OPENING) {
        return Vec::new();
    }
    let mut links = Vec::new();
    let mut segment_start = 0;
    for excluded in excluded_ranges(text) {
        find_in_segment(text, segment_start..excluded.start, &mut links);
        segment_start = segment_start.max(excluded.end);
    }
    find_in_segment(text, segment_start..text.len(), &mut links);
    links
}

/// Wikiリンクにしない範囲の外の本文の1区切りから、Wikiリンクを探して `links` に足す。
fn find_in_segment(text: &str, segment: Range<usize>, links: &mut Vec<Wikilink>) {
    if segment.start >= segment.end {
        return;
    }
    let mut pos = segment.start;
    while let Some(offset) = text[pos..segment.end].find(OPENING) {
        let start = pos + offset;
        let inner_start = start + OPENING.len();
        let line_end = text[inner_start..segment.end]
            .find(['\r', '\n'])
            .map_or(segment.end, |i| inner_start + i);
        let Some(inner_len) = text[inner_start..line_end].find(CLOSING) else {
            // この行には閉じる記号が無いため、行のどこから始めてもWikiリンクにならない。
            pos = line_end;
            continue;
        };
        let inner_end = inner_start + inner_len;
        let end = inner_end + CLOSING.len();
        let title = text[inner_start..inner_end].trim();
        if !title.is_empty() && !is_escaped(text, start) {
            links.push(Wikilink {
                range: start..end,
                title: title.to_owned(),
            });
        }
        pos = end;
    }
}

/// 入力中のWikiリンク。補完の候補を探すときに使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikilinkQuery {
    /// 入力中のWikiリンクの `[[` の位置。
    pub start: usize,
    /// `[[` からカーソルまでの文字列。ページを探すキーワードにする。
    pub keyword: String,
}

/// カーソルの前の本文から、入力中のWikiリンクを返す。
///
/// カーソルの行で、閉じていない `[[` の後ろにカーソルがあれば、入力中とする。
/// どの `[[` と `]]` が組になるかは、[`find_wikilinks`] と同じく行の頭から順に決める。
/// Wikiリンクは行をまたがないため、`text` にはカーソルの行だけを渡してもよい。
/// コードの中かどうかは見ない (行だけでは判定できないため)。
pub fn wikilink_query(text: &str, cursor: usize) -> Option<WikilinkQuery> {
    let before = &text[..cursor];
    let line_start = before.rfind(['\r', '\n']).map_or(0, |i| i + 1);
    let mut pos = line_start;
    while let Some(offset) = before[pos..].find(OPENING) {
        let start = pos + offset;
        let inner_start = start + OPENING.len();
        match before[inner_start..].find(CLOSING) {
            Some(inner_len) => pos = inner_start + inner_len + CLOSING.len(),
            None if is_escaped(text, start) => return None,
            None => {
                return Some(WikilinkQuery {
                    start,
                    keyword: before[inner_start..].to_owned(),
                });
            }
        }
    }
    None
}

/// 入力中のWikiリンクを、選んだタイトルのWikiリンクに置き換える編集を返す。
///
/// `[[` からカーソルまでを `[[タイトル]]` に置き換え、カーソルをその後ろに置く。
/// カーソルの直後に `]]` があれば (括弧の自動閉じで入ったものなど)、それも置き換える。
/// タイトルを同じリンク先のWikiリンクとして表せないときは `None` を返す。
pub fn complete_wikilink(
    text: &str,
    query: &WikilinkQuery,
    cursor: usize,
    title: &str,
) -> Option<TextEdit> {
    // タイトルのバッククォートは、同じ段落の前後のバッククォートと組になってインラインのコードになり、
    // 補完した位置やあとからの入力によってWikiリンクでなくなるため、補完しない。
    if title.contains('`') {
        return None;
    }
    let replacement = format!("{OPENING}{title}{CLOSING}");
    let links = find_wikilinks(&replacement);
    let [link] = links.as_slice() else {
        return None;
    };
    if link.range != (0..replacement.len()) || link.title != title {
        return None;
    }
    let end = if text[cursor..].starts_with(CLOSING) {
        cursor + CLOSING.len()
    } else {
        cursor
    };
    let after = query.start + replacement.len();
    Some(TextEdit {
        range: query.start..end,
        text: replacement,
        selection: after..after,
    })
}

/// `pos` の文字がバックスラッシュでエスケープされているか。
///
/// 直前に続くバックスラッシュの数が奇数ならエスケープされている。
/// 偶数なら、バックスラッシュどうしでエスケープし合っていて、`pos` の文字はそのまま記法になる。
fn is_escaped(text: &str, pos: usize) -> bool {
    text[..pos]
        .bytes()
        .rev()
        .take_while(|&byte| byte == b'\\')
        .count()
        % 2
        == 1
}

/// Wikiリンクにしない範囲を、開始位置の順に返す。範囲は重なることがある。
///
/// 今のWikinoの本文の描画と同じく、コード・Markdownのリンク (テキストとリンク先) と画像・参照先の定義・HTMLのタグと、
/// HTMLのa・code・pre・script・styleの要素の内容を除く。
/// 描画するときにWikiリンクをMarkdownのリンクへ置き換えても、リンクやHTMLの構文を壊さないためでもある。
///
/// HTMLブロックは、今のWikinoと違って、中の本文も含めて除く。
/// 描画の部品はHTMLブロックの中のMarkdownを解釈しないため、置き換えてもリンクにならないからである。
fn excluded_ranges(text: &str) -> Vec<Range<usize>> {
    // Wikinoの本文の描画と同じ拡張 (表・打ち消し線・タスクの記法) だけを有効にする。
    // 拡張が違うと、同じ本文でも範囲が変わる (表のセルの中のコードなど)。
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(text, options).into_offset_iter();
    let definitions: Vec<Range<usize>> = events
        .reference_definitions()
        .iter()
        .map(|(_, definition)| definition.span.clone())
        .collect();
    let mut ranges = definitions;
    let mut html = Vec::new();
    for (event, range) in events {
        match event {
            Event::Html(_) | Event::InlineHtml(_) => {
                html.push(range.clone());
                ranges.push(range);
            }
            Event::Code(_)
            | Event::Start(
                Tag::CodeBlock(_) | Tag::HtmlBlock | Tag::Link { .. } | Tag::Image { .. },
            ) => ranges.push(range),
            _ => {}
        }
    }
    ranges.extend(html::protected_ranges(text, html));
    ranges.sort_by_key(|range| range.start);
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(text: &str) -> Vec<String> {
        find_wikilinks(text)
            .into_iter()
            .map(|link| link.title)
            .collect()
    }

    #[test]
    fn wikiリンクの範囲とタイトルを返す() {
        let text = "前に[[はじめに]]と[[ 議事録 ]]がある";
        let links = find_wikilinks(text);
        assert_eq!(links.len(), 2, "Wikiリンクの数が違う: {links:?}");
        assert_eq!(&text[links[0].range.clone()], "[[はじめに]]", "範囲が違う");
        assert_eq!(links[0].title, "はじめに", "タイトルが違う");
        assert_eq!(&text[links[1].range.clone()], "[[ 議事録 ]]", "範囲が違う");
        assert_eq!(links[1].title, "議事録", "前後の空白を除いていない");
        assert_eq!(
            &text[links[0].opening_range()],
            "[[",
            "開く記号の範囲が違う"
        );
        assert_eq!(
            &text[links[0].closing_range()],
            "]]",
            "閉じる記号の範囲が違う"
        );
    }

    #[test]
    fn 最初の閉じる記号までを1つのwikiリンクにする() {
        assert_eq!(titles("[[a]]b]]"), ["a"], "最初の ]] で閉じていない");
        assert_eq!(titles("[[[a]]"), ["[a"], "最初の [[ から始めていない");
        assert_eq!(
            titles("[[a]][[b]]"),
            ["a", "b"],
            "隣り合うWikiリンクを分けていない"
        );
    }

    #[test]
    fn 改行をまたぐものと中身が空白だけのものはwikiリンクにしない() {
        assert!(
            titles("[[a\nb]]").is_empty(),
            "改行をまたいだWikiリンクがある"
        );
        assert!(
            titles("[[]] [[ 　]]").is_empty(),
            "中身が空白だけのWikiリンクがある"
        );
        assert_eq!(
            titles("[[閉じない\n[[次の行]]"),
            ["次の行"],
            "次の行のWikiリンクを見落とした"
        );
    }

    #[test]
    fn 改行の種類によらず行をまたぐwikiリンクを除外する() {
        for newline in ["\n", "\r\n", "\r"] {
            let text = format!("[[前{newline}後]]");
            assert!(
                find_wikilinks(&text).is_empty(),
                "{newline:?} をまたいだWikiリンクがある"
            );

            let prefix = format!("前の本文[[閉じない{newline}");
            let text = format!("{prefix}[[次の行]]");
            assert_eq!(
                find_wikilinks(&text),
                [Wikilink {
                    range: prefix.len()..text.len(),
                    title: "次の行".to_owned(),
                }],
                "{newline:?} の次の行のWikiリンクかバイト範囲が違う"
            );
        }
    }

    #[test]
    fn コードの中のものはwikiリンクにしない() {
        let text = "\
`[[インライン]]` と [[本文]]

```
[[フェンス]]
```

    [[インデント]]

| 表 | `[[セルのコード]]` |
| -- | -- |
| [[セル]] | x |

[^1]: 脚注に見える参照先の定義

    [[定義の後ろのインデント]]
";
        assert_eq!(
            titles(text),
            ["本文", "セル"],
            "コードの中のWikiリンクを返した"
        );
    }

    #[test]
    fn コードをまたぐ組はwikiリンクにせず_コードの後ろは探す() {
        assert!(
            titles("[[a `b]]` c").is_empty(),
            "コードをまたいだWikiリンクがある"
        );
        assert_eq!(
            titles("`[[` [[b]]"),
            ["b"],
            "コードの後ろのWikiリンクを見落とした"
        );
    }

    #[test]
    fn エスケープした開く記号はwikiリンクにしない() {
        assert!(
            titles("\\[[エスケープ]]").is_empty(),
            "エスケープしたWikiリンクがある"
        );
        assert_eq!(
            titles("\\\\[[バックスラッシュの後ろ]]"),
            ["バックスラッシュの後ろ"],
            "エスケープしたバックスラッシュの後ろのWikiリンクを見落とした"
        );
        assert_eq!(
            titles("\\[[エスケープ]] [[次]]"),
            ["次"],
            "エスケープしたものの後ろのWikiリンクを見落とした"
        );
    }

    #[test]
    fn リンクと画像とhtmlと参照先の定義の中はwikiリンクにしない() {
        for text in [
            "[テキストの[[a]]](https://example.com)",
            "[テキスト](https://example.com/[[a]])",
            "![[[a]]](image.png)",
            "<https://example.com/[[a]]>",
            "<span title=\"[[a]]\">文</span>",
            "<div>\n[[a]]\n</div>",
            "[参照]: https://example.com/[[a]]",
        ] {
            assert!(titles(text).is_empty(), "{text:?} にWikiリンクがある");
        }
        assert_eq!(
            titles("[リンク](https://example.com) と [[次]]"),
            ["次"],
            "リンクの後ろのWikiリンクを見落とした"
        );
        assert_eq!(
            titles("<b>太字</b>の[[中]]"),
            ["中"],
            "HTMLのタグの間のWikiリンクを見落とした"
        );
    }

    #[test]
    fn インラインhtmlのコードとリンクの内容はwikiリンクにしない() {
        for body in [
            "文<code>[[a]]</code>と[[次]]",
            "文<a href=\"https://example.com\">[[a]]</a>と[[次]]",
            "文<CODE class=\"a>b\"><b>[[a]]</b></CODE>と[[次]]",
            "文<a><code>[[a]]</code>[[b]]</a>と[[次]]",
            "文<pre>[[a]]</pre>と[[次]]",
            "文<script>const x = '<code>[[a]]';</script>と[[次]]",
            "文<style>/* <code>[[a]] */</style>と[[次]]",
            "文<script>'</scripture>'; [[a]]</SCRIPT >[[次]]",
            "文<code>[[a]]\n\n</code>\n\n[[次]]",
            "文<a/>[[a]]</a>と[[次]]",
        ] {
            assert_eq!(titles(body), ["次"], "{body:?} の保護範囲が違う");
        }
        for body in [
            "<code/>[[ページ1]]</code> と [[ページ2]]",
            "<pre/>[[ページ1]]</pre>\n\n[[ページ2]]",
        ] {
            assert_eq!(
                titles(body),
                ["ページ2"],
                "{body:?} の自己終了の記法の要素の内容を保護していない"
            );
        }
        assert!(
            titles("文<code>[[a]]").is_empty(),
            "閉じていないcodeの内容を変えた"
        );
        assert_eq!(
            titles("文<b>[[太字]]</b> ` <code> ` [[次]]"),
            ["太字", "次"],
            "通常の要素やコードとして書いたタグの外まで保護した"
        );
        for body in [
            "文<!-- <code> -->[[次]]",
            "文<!--\n<code>\n-->[[次]]",
            "文<span title='<code>'>[[次]]</span>",
            "文<code-example>[[次]]</code-example>",
        ] {
            assert_eq!(titles(body), ["次"], "{body:?} の通常の本文まで保護した");
        }
    }

    #[test]
    fn htmlブロックの地の文のタグにならない山括弧で_保護する要素の終わりを読み飛ばさない() {
        for body in [
            "<pre>\na < b\n</pre>\n\n[[次]]",
            "<pre>\nx <- y\n</pre>\n\n[[次]]",
            "<a href=\"https://example.com\">\n1 < 2\n</a>\n\n[[次]]",
            "<div>\n<code>a < b</code>\n</div>\n\n[[次]]",
        ] {
            assert_eq!(
                titles(body),
                ["次"],
                "{body:?} の保護する要素の後ろまで保護した"
            );
        }
    }

    fn query(marked: &str) -> Option<(usize, String)> {
        let cursor = marked.find('|').expect("カーソルの位置が無い");
        let text = marked.replacen('|', "", 1);
        wikilink_query(&text, cursor).map(|query| (query.start, query.keyword))
    }

    #[test]
    fn 閉じていない開く記号の後ろを入力中のwikiリンクにする() {
        assert_eq!(
            query("前に[[はじ|"),
            Some((6, "はじ".to_owned())),
            "キーワードが違う"
        );
        assert_eq!(
            query("[[|]]"),
            Some((0, String::new())),
            "空のキーワードを返さない"
        );
        assert_eq!(
            query("[[a]] と [[b c|"),
            Some((10, "b c".to_owned())),
            "閉じたWikiリンクの後ろの入力中のものを返さない"
        );
        assert_eq!(
            query("[[[a|"),
            Some((0, "[a".to_owned())),
            "行の頭から組を決めていない"
        );
        assert_eq!(
            query("前の行[[\n次の[[行|"),
            Some((18, "行".to_owned())),
            "前の行の開く記号を使った"
        );
    }

    #[test]
    fn 閉じたものとエスケープしたものと行をまたぐものは入力中にしない() {
        for marked in [
            "[[a]]|",
            "[[a]] b|",
            "\\[[a|",
            "[[a\n|",
            "[[a\r\nb|",
            "[|",
            "ふつうの文|",
        ] {
            assert_eq!(query(marked), None, "{marked:?} を入力中にした");
        }
    }

    #[test]
    fn 候補を選ぶと開く記号からカーソルまでをwikiリンクに置き換える() {
        let text = "前に[[はじ]]後";
        let cursor = "前に[[はじ".len();
        let query = wikilink_query(text, cursor).expect("入力中のWikiリンクが無い");
        let edit = complete_wikilink(text, &query, cursor, "はじめに")
            .expect("通常のタイトルを補完できない");
        let mut result = text.to_owned();
        result.replace_range(edit.range.clone(), &edit.text);
        assert_eq!(
            result, "前に[[はじめに]]後",
            "閉じる記号ごと置き換えていない"
        );
        assert_eq!(
            edit.selection,
            "前に[[はじめに]]".len().."前に[[はじめに]]".len(),
            "カーソルを閉じる記号の後ろに置いていない"
        );

        let text = "[[はじ 後";
        let cursor = "[[はじ".len();
        let query = wikilink_query(text, cursor).expect("入力中のWikiリンクが無い");
        let edit = complete_wikilink(text, &query, cursor, "はじめに")
            .expect("通常のタイトルを補完できない");
        let mut result = text.to_owned();
        result.replace_range(edit.range, &edit.text);
        assert_eq!(result, "[[はじめに]] 後", "閉じる記号を足していない");
    }

    #[test]
    fn 元のタイトルとして解析できない候補は補完しない() {
        let text = "[[";
        let query = wikilink_query(text, text.len()).expect("入力中のWikiリンクが無い");
        for title in [
            "",
            " ",
            "　",
            "議事録]]追記",
            "名前]",
            "前\n後",
            "前\r後",
            " 前",
            "後 ",
            "`コード`",
            "a`b",
        ] {
            assert!(
                complete_wikilink(text, &query, text.len(), title).is_none(),
                "元のリンク先として表せない {title:?} を補完した"
            );
        }
    }

    #[test]
    fn 引用やリストの中のものもwikiリンクにする() {
        assert_eq!(
            titles("> [[引用]]\n- [[箇条書き]]\n# [[見出し]]"),
            ["引用", "箇条書き", "見出し"],
            "ブロックの中のWikiリンクを見落とした"
        );
    }
}
