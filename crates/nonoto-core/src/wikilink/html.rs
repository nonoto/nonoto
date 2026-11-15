//! Markdownのパーサーが認識したHTMLを起点に、Wikiリンクにしない要素の範囲を集める。

use std::ops::Range;

/// タグそのものに加え、Wikinoが保護するa・code・pre・script・styleの内容も保護する。
pub(super) fn protected_ranges(text: &str, ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    // 複数行のコメントやタグがイベントで分かれても、途中の文字を別のタグとして読まない。
    let mut joined: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = joined.last_mut()
            && last.end == range.start
        {
            last.end = range.end;
        } else {
            joined.push(range);
        }
    }
    let mut protected = Vec::new();
    let mut open: Vec<(String, usize)> = Vec::new();
    let mut raw_end = 0;
    for range in joined {
        let mut pos = range.start.max(raw_end);
        while pos < range.end {
            let Some(offset) = text[pos..range.end].find('<') else {
                break;
            };
            pos += offset;
            if text[pos..].starts_with("<!--") {
                pos = text[pos + 4..range.end]
                    .find("-->")
                    .map_or(range.end, |offset| pos + 4 + offset + 3);
                continue;
            }
            if !starts_markup(&text[pos..]) {
                // HTMLブロックの地の文の `a < b` のように、タグにならない `<` は文字として読む。
                pos += 1;
                continue;
            }
            let start = pos;
            let Some(end) = tag_end(text, start, range.end) else {
                break;
            };
            pos = end;
            let token = &text[start + 1..end - 1];
            let closing = token.starts_with('/');
            let name = token.trim_start_matches('/');
            let name_end = name
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .unwrap_or(name.len());
            let name = name[..name_end].to_ascii_lowercase();
            if !matches!(name.as_str(), "a" | "code" | "pre" | "script" | "style") {
                continue;
            }
            if closing {
                if let Some(index) = open.iter().rposition(|(tag, _)| *tag == name) {
                    let (_, start) = open.remove(index);
                    protected.push(start..end);
                }
            } else if matches!(name.as_str(), "script" | "style") {
                if !token.trim_end().ends_with('/') {
                    // raw textの中のタグやMarkdownの記号は、対応する終了タグまで文字として読む。
                    raw_end = raw_text_end(text, end, &name);
                    protected.push(start..raw_end);
                    pos = raw_end;
                }
            } else {
                // a・code・preは空要素でないため、HTMLと同じく `/>` で書いても開始タグとして読む。
                open.push((name, start));
            }
        }
    }
    protected.extend(open.into_iter().map(|(_, start)| start..text.len()));
    protected
}

/// `<` から始まる `rest` が、タグかコメントなどの始まりか。
///
/// HTMLのトークナイザーと同じく、`<` の直後が英字・`/` と英字・`!`・`?` のときだけ始まりとして読む。
fn starts_markup(rest: &str) -> bool {
    let bytes = rest.as_bytes();
    match bytes.get(1) {
        Some(byte) if byte.is_ascii_alphabetic() => true,
        Some(b'/') => bytes.get(2).is_some_and(u8::is_ascii_alphabetic),
        Some(b'!' | b'?') => true,
        _ => false,
    }
}

/// 属性の引用符の中の `>` はタグを終えない。
fn tag_end(text: &str, start: usize, limit: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, byte) in text.as_bytes()[start + 1..limit]
        .iter()
        .copied()
        .enumerate()
    {
        match (quote, byte) {
            (Some(current), byte) if current == byte => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(start + 1 + offset + 1),
            _ => {}
        }
    }
    None
}

fn raw_text_end(text: &str, mut pos: usize, name: &str) -> usize {
    while let Some(offset) = text[pos..].find("</") {
        let start = pos + offset;
        let name_start = start + 2;
        let name_end = name_start + name.len();
        if text
            .as_bytes()
            .get(name_start..name_end)
            .is_some_and(|part| part.eq_ignore_ascii_case(name.as_bytes()))
            && text
                .as_bytes()
                .get(name_end)
                .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'))
            && let Some(end) = tag_end(text, start, text.len())
        {
            return end;
        }
        pos = name_start;
    }
    text.len()
}
