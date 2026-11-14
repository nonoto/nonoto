//! 本文の編集の操作 (リストの継続とインデント) のロジック。
//!
//! 文字列だけで決まる処理をUIから分け、どのUIでも同じ挙動にする。
//! 規則は今のWikinoのエディタに揃える。
//! 位置は、入力の部品と同じくUTF-8のバイト位置で扱う。

use std::ops::Range;

/// インデントの1段。半角スペース2つにする。
const INDENT: &str = "  ";

/// 本文の1か所の置き換えと、置き換えたあとの選択。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    /// 置き換える範囲。置き換える前の本文の位置で表す。
    pub range: Range<usize>,
    /// 置き換える文字列。
    pub text: String,
    /// 置き換えたあとの選択。置き換えたあとの本文の位置で表す。
    pub selection: Range<usize>,
}

/// リストの項目の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListKind {
    /// 箇条書き。記号 (`-`・`*`・`+`) を持つ。
    Bullet(char),
    /// タスク。箇条書きの記号を持つ。
    Task(char),
    /// 番号付き。番号を持つ。
    Ordered(u64),
}

/// 1行のリストの項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ListItem<'a> {
    /// 行頭の空白。
    indent: &'a str,
    kind: ListKind,
    /// 記号と後ろの空白 (タスクではチェックボックスと後ろの空白も) の終わり。行の中の位置で表す。
    marker_end: usize,
    /// 記号の後ろの内容。
    content: &'a str,
}

impl ListItem<'_> {
    /// 次の項目の書き出し。番号付きでは番号を1つ進め、タスクは未完了にする。
    fn next_prefix(&self, indent: &str) -> String {
        match self.kind {
            ListKind::Bullet(mark) => format!("{indent}{mark} "),
            ListKind::Task(mark) => format!("{indent}{mark} [ ] "),
            ListKind::Ordered(number) => format!("{indent}{}. ", number.saturating_add(1)),
        }
    }

    /// 1段浅くした、内容の無い項目の書き出し。番号付きは1から始め直す。
    fn outdented_prefix(&self, indent: &str) -> String {
        match self.kind {
            ListKind::Bullet(mark) => format!("{indent}{mark} "),
            ListKind::Task(mark) => format!("{indent}{mark} [ ] "),
            ListKind::Ordered(_) => format!("{indent}1. "),
        }
    }
}

/// Enterで改行するときに、リストを続ける編集を返す。
///
/// カーソルがリストの項目の記号より後ろにあれば、改行して次の項目の書き出しを入れる。
/// 内容が空の項目では、改行せずに記号を消す。字下げした項目では、消す代わりに1段浅くする。
/// リストの項目でない行や、カーソルが記号の途中にあるときは `None` を返し、ふつうに改行させる。
pub fn continue_list(text: &str, cursor: usize) -> Option<TextEdit> {
    let line = line_range(text, cursor);
    let item = parse_list_item(&text[line.clone()])?;
    if cursor - line.start < item.marker_end {
        return None;
    }
    if item.content.trim().is_empty() {
        let replacement = match item.indent.len().checked_sub(INDENT.len()) {
            Some(depth) => item.outdented_prefix(&" ".repeat(depth)),
            None => String::new(),
        };
        let end = line.start + replacement.len();
        return Some(TextEdit {
            range: line,
            text: replacement,
            selection: end..end,
        });
    }
    let inserted = format!("\n{}", item.next_prefix(item.indent));
    let end = cursor + inserted.len();
    Some(TextEdit {
        range: cursor..cursor,
        text: inserted,
        selection: end..end,
    })
}

/// Tabで、インデントを1段足す編集を返す。
///
/// 範囲を選んでいれば、選んだ行の行頭に足す。
/// 選んでいなければ、リストの項目の記号の直後では行頭に足し、それ以外ではカーソルの位置に足す。
pub fn indent(text: &str, selection: Range<usize>) -> TextEdit {
    if selection.is_empty() {
        let cursor = selection.start;
        let line = line_range(text, cursor);
        let at = match parse_list_item(&text[line.clone()]) {
            Some(item) if cursor - line.start == item.marker_end => line.start,
            _ => cursor,
        };
        let end = cursor + INDENT.len();
        return TextEdit {
            range: at..at,
            text: INDENT.to_owned(),
            selection: end..end,
        };
    }
    let lines = selected_lines(text, &selection);
    let block = lines.first().map_or(0, |line| line.start)..lines.last().map_or(0, |line| line.end);
    let mut replacement = String::with_capacity(block.len() + lines.len() * INDENT.len());
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            replacement.push_str(&text[lines[i - 1].end..line.start]);
        }
        replacement.push_str(INDENT);
        replacement.push_str(&text[line.clone()]);
    }
    let end = selection.end + lines.len() * INDENT.len();
    TextEdit {
        range: block.clone(),
        text: replacement,
        selection: block.start..end,
    }
}

/// Shift-Tabで、選んだ行 (選んでいなければカーソルの行) のインデントを1段削る編集を返す。
///
/// 行頭の半角スペースを2つまで削る。削る空白が無ければ `None` を返す。
pub fn outdent(text: &str, selection: Range<usize>) -> Option<TextEdit> {
    let lines = selected_lines(text, &selection);
    let removals: Vec<Range<usize>> = lines
        .iter()
        .map(|line| {
            let spaces = text[line.clone()]
                .bytes()
                .take(INDENT.len())
                .take_while(|&byte| byte == b' ')
                .count();
            line.start..line.start + spaces
        })
        .collect();
    if removals.iter().all(Range::is_empty) {
        return None;
    }
    let block = lines.first().map_or(0, |line| line.start)..lines.last().map_or(0, |line| line.end);
    let mut replacement = String::with_capacity(block.len());
    let mut pos = block.start;
    for removal in &removals {
        replacement.push_str(&text[pos..removal.start]);
        pos = removal.end;
    }
    replacement.push_str(&text[pos..block.end]);
    let map = |offset: usize| {
        let removed: usize = removals
            .iter()
            .map(|removal| offset.clamp(removal.start, removal.end) - removal.start)
            .sum();
        offset - removed
    };
    Some(TextEdit {
        range: block,
        text: replacement,
        selection: map(selection.start)..map(selection.end),
    })
}

/// `offset` を含む行の範囲。改行 (CR・LF・CRLF) を含めない。
fn line_range(text: &str, offset: usize) -> Range<usize> {
    // CRLFの途中の位置は、改行の前の行に含める。
    let offset = if offset > 0
        && text.as_bytes()[offset - 1] == b'\r'
        && text.as_bytes().get(offset) == Some(&b'\n')
    {
        offset - 1
    } else {
        offset
    };
    let start = text[..offset].rfind(['\r', '\n']).map_or(0, |i| i + 1);
    let end = text[offset..]
        .find(['\r', '\n'])
        .map_or(text.len(), |i| offset + i);
    start..end
}

/// 選んだ範囲が掛かる行の範囲を、上から順に返す。
///
/// 選んだ範囲の終わりが行頭にあるときは、その行を含めない (行を丸ごと選んだときに次の行まで変えないため)。
fn selected_lines(text: &str, selection: &Range<usize>) -> Vec<Range<usize>> {
    let first = line_range(text, selection.start);
    let mut last_offset = selection.end;
    if selection.end > first.end && line_range(text, selection.end).start == selection.end {
        last_offset = selection.end - 1;
    }
    let mut lines = vec![first.clone()];
    let mut line = first;
    while line.end < last_offset {
        let next_start = match text.as_bytes().get(line.end) {
            Some(b'\r') if text.as_bytes().get(line.end + 1) == Some(&b'\n') => line.end + 2,
            Some(b'\r' | b'\n') => line.end + 1,
            _ => break,
        };
        if next_start > last_offset {
            break;
        }
        let next = line_range(text, next_start);
        if next.start <= line.start {
            break;
        }
        line = next;
        lines.push(line.clone());
    }
    lines
}

/// 行がリストの項目なら、その中身を返す。
///
/// 箇条書き (`-`・`*`・`+`)・タスク (`- [ ]`・`- [x]`)・番号付き (`1.`) を、記号の後ろに空白があるときだけ項目にする。
fn parse_list_item(line: &str) -> Option<ListItem<'_>> {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let indent = &line[..indent_len];
    let rest = &line[indent_len..];
    let (kind, after_marker) = match rest.chars().next()? {
        mark @ ('-' | '*' | '+') => (ListKind::Bullet(mark), indent_len + 1),
        '0'..='9' => {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            // CommonMarkでは、番号は9桁までである。
            if digits > 9 || rest.as_bytes().get(digits) != Some(&b'.') {
                return None;
            }
            let number = rest[..digits].parse().ok()?;
            (ListKind::Ordered(number), indent_len + digits + 1)
        }
        _ => return None,
    };
    let content_start = skip_spaces(line, after_marker)?;
    if let ListKind::Bullet(mark) = kind
        && let Some(box_end) = task_box_end(line, content_start)
        && let Some(task_content_start) = skip_spaces(line, box_end)
    {
        return Some(ListItem {
            indent,
            kind: ListKind::Task(mark),
            marker_end: task_content_start,
            content: &line[task_content_start..],
        });
    }
    Some(ListItem {
        indent,
        kind,
        marker_end: content_start,
        content: &line[content_start..],
    })
}

/// `from` から続く空白を飛ばした位置。空白が1つも無ければ `None` を返す。
fn skip_spaces(line: &str, from: usize) -> Option<usize> {
    let spaces = line[from..].len() - line[from..].trim_start_matches([' ', '\t']).len();
    (spaces > 0).then_some(from + spaces)
}

/// `from` にタスクのチェックボックス (`[ ]`・`[x]`・`[X]`) があれば、その終わりの位置を返す。
fn task_box_end(line: &str, from: usize) -> Option<usize> {
    let rest = &line.as_bytes()[from..];
    (rest.len() >= 3 && rest[0] == b'[' && matches!(rest[1], b' ' | b'x' | b'X') && rest[2] == b']')
        .then_some(from + 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本文の `|` をカーソルの位置として取り除き、本文とカーソルの位置を返す。
    fn with_cursor(marked: &str) -> (String, usize) {
        let cursor = marked.find('|').expect("カーソルの位置が無い");
        (marked.replacen('|', "", 1), cursor)
    }

    /// 本文の `<` と `>` を選択の始めと終わりとして取り除き、本文と選択を返す。
    fn with_selection(marked: &str) -> (String, Range<usize>) {
        let start = marked.find('<').expect("選択の始めが無い");
        let end = marked.find('>').expect("選択の終わりが無い") - 1;
        (marked.replacen('<', "", 1).replacen('>', "", 1), start..end)
    }

    /// 編集を当てたあとの本文に、選択を `<` と `>` (空なら `|`) で書き込む。
    fn applied(text: &str, edit: &TextEdit) -> String {
        let mut result = text.to_owned();
        result.replace_range(edit.range.clone(), &edit.text);
        let Range { start, end } = edit.selection.clone();
        if start == end {
            result.insert(start, '|');
        } else {
            result.insert(end, '>');
            result.insert(start, '<');
        }
        result
    }

    fn enter(marked: &str) -> Option<String> {
        let (text, cursor) = with_cursor(marked);
        continue_list(&text, cursor).map(|edit| applied(&text, &edit))
    }

    #[test]
    fn リストの項目の後ろで改行すると次の項目を書き出す() {
        assert_eq!(enter("- a|").as_deref(), Some("- a\n- |"), "箇条書き");
        assert_eq!(
            enter("* a|").as_deref(),
            Some("* a\n* |"),
            "記号を保っていない"
        );
        assert_eq!(
            enter("+ a|").as_deref(),
            Some("+ a\n+ |"),
            "記号を保っていない"
        );
        assert_eq!(
            enter("- [x] 済み|").as_deref(),
            Some("- [x] 済み\n- [ ] |"),
            "タスクを未完了で続けていない"
        );
        assert_eq!(
            enter("9. a|").as_deref(),
            Some("9. a\n10. |"),
            "番号を進めていない"
        );
        assert_eq!(
            enter("前の行\n  - 字下げ|\n後の行").as_deref(),
            Some("前の行\n  - 字下げ\n  - |\n後の行"),
            "字下げを保っていない"
        );
    }

    #[test]
    fn 内容の途中で改行するとカーソルの後ろを次の項目に移す() {
        assert_eq!(
            enter("- 前|後").as_deref(),
            Some("- 前\n- |後"),
            "カーソルの後ろの内容を移していない"
        );
    }

    #[test]
    fn 内容が空の項目では記号を消し_字下げした項目では1段浅くする() {
        assert_eq!(
            enter("- a\n- |").as_deref(),
            Some("- a\n|"),
            "記号を消していない"
        );
        assert_eq!(
            enter("- [ ] |").as_deref(),
            Some("|"),
            "タスクの記号を消していない"
        );
        assert_eq!(enter("1.  |").as_deref(), Some("|"), "番号を消していない");
        assert_eq!(
            enter("    - |\n次").as_deref(),
            Some("  - |\n次"),
            "1段浅くしていない"
        );
        assert_eq!(
            enter("  3. |").as_deref(),
            Some("1. |"),
            "浅くした番号付きを1から始めていない"
        );
    }

    #[test]
    fn リストの項目でない行と記号の途中ではふつうに改行させる() {
        for marked in [
            "ふつうの文|",
            "-a|",
            "---|",
            "1)a|",
            "1234567890. a|",
            "|- a",
            "-| a",
            "# 見出し|",
        ] {
            assert_eq!(enter(marked), None, "{marked:?} でリストを続けた");
        }
        assert_eq!(
            enter("- [ ]|").as_deref(),
            Some("- [ ]\n- |"),
            "後ろに空白の無いチェックボックスは内容として扱う"
        );
    }

    #[test]
    fn crlfの行でも行の終わりを改行の前にする() {
        assert_eq!(
            enter("- a|\r\n次").as_deref(),
            Some("- a\n- |\r\n次"),
            "CRLFの行で続けられない"
        );
        assert_eq!(enter("- |\r\n次").as_deref(), Some("|\r\n次"), "CRを消した");
    }

    fn tab(marked: &str) -> String {
        let (text, selection) = if marked.contains('|') {
            let (text, cursor) = with_cursor(marked);
            (text, cursor..cursor)
        } else {
            with_selection(marked)
        };
        applied(&text, &indent(&text, selection))
    }

    #[test]
    fn tabでカーソルの位置か_リストの記号の直後では行頭に字下げを足す() {
        assert_eq!(tab("ab|c"), "ab  |c", "カーソルの位置に足していない");
        assert_eq!(tab("- |"), "  - |", "空の項目で行頭に足していない");
        assert_eq!(
            tab("- [ ] |内容"),
            "  - [ ] |内容",
            "記号の直後で行頭に足していない"
        );
        assert_eq!(
            tab("- 内|容"),
            "- 内  |容",
            "内容の途中でカーソルの位置に足していない"
        );
    }

    #[test]
    fn tabで選んだ行の行頭に字下げを足す() {
        assert_eq!(
            tab("前\n- <a\n- b>\n後"),
            "前\n<  - a\n  - b>\n後",
            "選んだ行に足していない"
        );
        assert_eq!(
            tab("<a\n>b"),
            "<  a\n>b",
            "行頭で終わる選択の、次の行にも足した"
        );
    }

    fn shift_tab(marked: &str) -> Option<String> {
        let (text, selection) = if marked.contains('|') {
            let (text, cursor) = with_cursor(marked);
            (text, cursor..cursor)
        } else {
            with_selection(marked)
        };
        outdent(&text, selection).map(|edit| applied(&text, &edit))
    }

    #[test]
    fn shift_tabで選んだ行の字下げを2つまで削る() {
        assert_eq!(
            shift_tab("    - a|").as_deref(),
            Some("  - a|"),
            "カーソルの行の字下げを削っていない"
        );
        assert_eq!(
            shift_tab("<    a\n b\nc>").as_deref(),
            Some("<  a\nb\nc>"),
            "選んだ行の字下げを削っていない"
        );
        assert_eq!(
            shift_tab(" | a").as_deref(),
            Some("|a"),
            "削った字下げの中のカーソルを行頭に寄せていない"
        );
        assert_eq!(shift_tab("a|"), None, "字下げの無い行を変えた");
        assert_eq!(shift_tab("\ta|"), None, "タブ文字を削った");
    }

    #[test]
    fn 改行の種類によらず末尾までの字下げを編集できる() {
        for newline in ["\r", "\n", "\r\n"] {
            let selected = format!("<  前{newline}  後{newline}>");
            assert_eq!(
                tab(&selected),
                format!("<    前{newline}    後{newline}>"),
                "{newline:?} の末尾まで選ぶとTabの対象行が違う"
            );
            assert_eq!(
                shift_tab(&selected),
                Some(format!("<前{newline}後{newline}>")),
                "{newline:?} の末尾まで選ぶとShift-Tabの対象行が違う"
            );

            let cursor = format!("  前{newline}|");
            assert_eq!(
                tab(&cursor),
                format!("  前{newline}  |"),
                "{newline:?} の末尾の空行にTabで字下げを足せない"
            );
            assert_eq!(shift_tab(&cursor), None, "{newline:?} の末尾の空行を変えた");

            let first_line = format!("<  前{newline}>  後");
            assert_eq!(
                tab(&first_line),
                format!("<    前{newline}>  後"),
                "{newline:?} の次の行にもTabで字下げを足した"
            );
            assert_eq!(
                shift_tab(&first_line),
                Some(format!("<前{newline}>  後")),
                "{newline:?} の次の行もShift-Tabで変えた"
            );
        }

        assert_eq!(
            tab("<a\r>\nb"),
            "<  a\r>\nb",
            "CRLFの途中で選択を終えると次の行も変えた"
        );
        assert_eq!(
            shift_tab("<  a\r>\nb").as_deref(),
            Some("<a\r>\nb"),
            "CRLFの途中で選択を終えると次の行も変えた"
        );
    }

    #[test]
    fn 改行の種類によらずリストの行だけを編集する() {
        for newline in ["\r", "\n", "\r\n"] {
            assert_eq!(
                enter(&format!("- 前|{newline}後")),
                Some(format!("- 前\n- |{newline}後")),
                "{newline:?} の前でリストを続けられない"
            );
            assert_eq!(
                enter(&format!("- |{newline}後")),
                Some(format!("|{newline}後")),
                "{newline:?} の前の空の項目を消せない"
            );
            assert_eq!(
                enter(&format!("- 前{newline}後|")),
                None,
                "{newline:?} の次の行をリストとして扱った"
            );
        }
    }
}
