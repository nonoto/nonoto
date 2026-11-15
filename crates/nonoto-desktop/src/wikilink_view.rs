//! 描画した表示のWikiリンク。
//!
//! 描画の部品はWikiリンクの記法を知らないため、描画に渡す前に、コアの解析で見つけたWikiリンクを
//! アプリの中だけで使うスキームのMarkdownのリンクに置き換える。
//! リンクには本文の中の番号だけを持たせ、押したときに番号からリンク先のページを引く。

use std::collections::HashMap;

use nonoto_core::{PageId, find_wikilinks};

/// 表示のWikiリンクに使う、アプリの中だけのリンクのスキーム。
const SCHEME: &str = "nonoto-wikilink:";

/// 描画に渡すMarkdownと、Wikiリンクの番号ごとのリンク先。
#[derive(Debug, PartialEq, Eq)]
pub struct RenderedBody {
    pub markdown: String,
    pub targets: Vec<PageId>,
}

/// 本文のWikiリンクを置き換えたMarkdownを作る。
///
/// リンク先のページがあるWikiリンクは、タイトルをテキストにしたアプリの中だけのスキームのリンクにする。
/// リンク先が無いWikiリンクは、押しても移らないよう、リンクにせず、括弧も含めて書いたとおりの文字にする。
/// `resolve` は同じタイトルについて1回だけ呼ぶ。
pub fn render(body: &str, mut resolve: impl FnMut(&str) -> Option<PageId>) -> RenderedBody {
    let links = find_wikilinks(body);
    let mut markdown = String::with_capacity(body.len());
    let mut targets = Vec::new();
    let mut resolved: HashMap<&str, Option<PageId>> = HashMap::new();
    let mut copied = 0;
    for link in &links {
        markdown.push_str(&body[copied..link.range.start]);
        let target = *resolved
            .entry(link.title.as_str())
            .or_insert_with(|| resolve(&link.title));
        match target {
            Some(id) => {
                // 直前の `!` と合わさると画像になるため、表示する文字を保って記法だけを外す。
                if body[..link.range.start].ends_with('!') {
                    let slashes = body[..link.range.start - 1]
                        .bytes()
                        .rev()
                        .take_while(|&byte| byte == b'\\')
                        .count();
                    if slashes.is_multiple_of(2) {
                        markdown.pop();
                        markdown.push_str("\\!");
                    }
                }
                markdown.push('[');
                push_escaped(&mut markdown, &link.title);
                markdown.push_str("](");
                markdown.push_str(SCHEME);
                markdown.push_str(&targets.len().to_string());
                markdown.push(')');
                targets.push(id);
            }
            // 括弧を外すと、隣り合う強調の記号 (`**` など) の解釈が変わるため、記号を消さない。
            None => push_escaped(&mut markdown, &body[link.range.clone()]),
        }
        copied = link.range.end;
    }
    markdown.push_str(&body[copied..]);
    RenderedBody { markdown, targets }
}

/// アプリの中だけのスキームのリンクなら、Wikiリンクの番号を返す。
pub fn target_index(url: &str) -> Option<usize> {
    url.strip_prefix(SCHEME)?.parse().ok()
}

/// 文字列を、Markdownの記法として読まれない文字列にして足す。
///
/// CommonMarkでは、ASCIIの記号はどれもバックスラッシュでエスケープでき、エスケープした記号はその文字のまま表示される。
/// 記号をすべてエスケープすれば、強調・リンク・HTML・行頭の見出しやリストの記号などに読まれない。
fn push_escaped(markdown: &mut String, text: &str) {
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            markdown.push('\\');
        }
        markdown.push(c);
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::base::text::editor_host::{BlockNode, parse_markdown};

    use super::*;

    fn page_ids(count: usize) -> Vec<PageId> {
        (0..count).map(|_| PageId::new()).collect()
    }

    #[test]
    fn リンク先のあるwikiリンクを番号のリンクにし_無いものは書いたとおりの文字にする() {
        let ids = page_ids(2);
        let rendered = render(
            "前に[[はじめに]]と[[無い]]と[[議事録]]と[[はじめに]]",
            |title| match title {
                "はじめに" => Some(ids[0]),
                "議事録" => Some(ids[1]),
                _ => None,
            },
        );
        assert_eq!(
            rendered.markdown,
            "前に[はじめに](nonoto-wikilink:0)と\\[\\[無い\\]\\]と[議事録](nonoto-wikilink:1)と[はじめに](nonoto-wikilink:2)",
            "置き換えた本文が期待と異なる"
        );
        assert_eq!(
            rendered.targets,
            [ids[0], ids[1], ids[0]],
            "番号ごとのリンク先が期待と異なる"
        );
        assert_eq!(target_index("nonoto-wikilink:2"), Some(2), "番号を読めない");
    }

    #[test]
    fn 同じタイトルのリンク先は1回だけ探す() {
        let mut calls = Vec::new();
        render("[[a]] [[b]] [[a]]", |title| {
            calls.push(title.to_owned());
            None
        });
        assert_eq!(calls, ["a", "b"], "同じタイトルを何度も探した");
    }

    #[test]
    fn タイトルの記号はエスケープして記法として読ませない() {
        let id = PageId::new();
        let rendered = render("[[*強調* a<b & [x] \\ 1.]]\n[[# 見出し]]", |_| {
            Some(id)
        });
        assert_eq!(
            rendered.markdown,
            "[\\*強調\\* a\\<b \\& \\[x\\] \\\\ 1\\.](nonoto-wikilink:0)\n[\\# 見出し](nonoto-wikilink:1)",
            "タイトルの記号をエスケープしていない"
        );
        let single = render("[[*強調* a<b & [x] \\ 1.]]", |_| Some(id));
        assert_parsed_link(&single.markdown, "*強調* a<b & [x] \\ 1.");
        let broken = render("[[# 見出し]]", |_| None);
        assert_eq!(
            broken.markdown, "\\[\\[\\# 見出し\\]\\]",
            "リンク先が無いWikiリンクの記号をエスケープしていない"
        );
    }

    #[test]
    fn リンク先が無いwikiリンクは_前後の強調の記号の解釈を変えない() {
        for body in [
            "これは**[[ページ]]**です",
            "a*[[b]]*c",
            "文_[[ページ]]_文",
            "![[a]]",
        ] {
            let rendered = render(body, |_| None);
            assert_parsed_text(&rendered.markdown, body);
        }
    }

    /// 表示用のMarkdownが、強調・リンク・画像の無い1つの段落になり、表示する文字が `expected_text` になることを確かめる。
    fn assert_parsed_text(markdown: &str, expected_text: &str) {
        let nodes =
            parse_markdown(markdown, Default::default()).expect("表示用のMarkdownを解析できない");
        let [BlockNode::Paragraph(paragraph)] = nodes.as_slice() else {
            panic!("表示用のMarkdownが段落にならない: {nodes:?}");
        };
        let mut text = String::new();
        for inline in paragraph.children() {
            assert!(inline.as_image().is_none(), "{markdown:?} が画像になった");
            text.push_str(inline.text());
            for (_, mark) in inline.mark_runs() {
                assert!(
                    !mark.bold && !mark.italic && mark.link.is_none(),
                    "{markdown:?} に強調かリンクが付いた"
                );
            }
        }
        assert_eq!(text, expected_text, "表示する文字が書いた本文と異なる");
    }

    fn assert_parsed_link(markdown: &str, expected_text: &str) {
        let nodes =
            parse_markdown(markdown, Default::default()).expect("表示用のMarkdownを解析できない");
        let [BlockNode::Paragraph(paragraph)] = nodes.as_slice() else {
            panic!("表示用のMarkdownが段落にならない: {nodes:?}");
        };
        let mut text = String::new();
        let mut urls = Vec::new();
        for inline in paragraph.children() {
            assert!(inline.as_image().is_none(), "Wikiリンクが画像になった");
            text.push_str(inline.text());
            for (_, mark) in inline.mark_runs() {
                if let Some(link) = &mark.link {
                    urls.push(link.url.as_ref());
                }
            }
        }
        assert_eq!(text, expected_text, "タイトルか周囲の表示文字が変わった");
        assert_eq!(urls, ["nonoto-wikilink:0"], "Wikiリンクのリンク先が違う");
    }

    #[test]
    fn 感嘆符の直後も画像にせずwikiリンクにする() {
        for (body, expected) in [
            ("![[はじめに]]", "!はじめに"),
            (r"\![[はじめに]]", "!はじめに"),
            (r"\\![[はじめに]]", r"\!はじめに"),
            ("!![[はじめに]]", "!!はじめに"),
            ("前![[はじめに]]後", "前!はじめに後"),
        ] {
            let rendered = render(body, |_| Some(PageId::new()));
            assert_parsed_link(&rendered.markdown, expected);
        }
    }

    #[test]
    fn wikiリンクでない記法とほかのリンクはそのまま残す() {
        for body in [
            "`[[コード]]` [リンク](https://example.com/[[a]]) \\[[エスケープ]]",
            "文<code>[[はじめに]]</code>",
            "文<a href=\"https://example.com\">[[はじめに]]</a>",
        ] {
            let rendered = render(body, |_| Some(PageId::new()));
            assert_eq!(rendered.markdown, body, "Wikiリンクでない記法を書き換えた");
            assert!(rendered.targets.is_empty(), "リンク先を持った");
        }
        assert_eq!(
            target_index("https://example.com"),
            None,
            "ほかのリンクを番号として読んだ"
        );
        assert_eq!(
            target_index("nonoto-wikilink:x"),
            None,
            "番号でないものを番号として読んだ"
        );
    }
}
