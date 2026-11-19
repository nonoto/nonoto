//! 本文の入力で、Markdownの記法を装飾する。
//!
//! 記法の解析は `gpui-component` のtree-sitterのハイライトに任せ、このモジュールでは
//! どの記法をどのハイライトの名前にするか (クエリ) と、名前ごとの見た目 (テーマのスタイル) を決める。

use std::rc::Rc;

use gpui_kit::component::highlighter::{LanguageRegistry, SyntaxColors, ThemeStyle};
use gpui_kit::component::{Theme, ThemeColor, ThemeConfig};
use gpui_kit::{App, Hsla};

/// 本文の入力に設定する言語の名前。
pub const LANGUAGE: &str = "markdown";
/// 段落や見出しの中の記法を解析する言語の名前。ブロックの言語の注入のクエリから参照する。
const INLINE_LANGUAGE: &str = "markdown_inline";

const BLOCK_HIGHLIGHTS: &str = include_str!("../queries/markdown/highlights.scm");
const BLOCK_INJECTIONS: &str = include_str!("../queries/markdown/injections.scm");
const INLINE_HIGHLIGHTS: &str = include_str!("../queries/markdown_inline/highlights.scm");

/// 部品に組み込まれたMarkdownの文法を使い、ハイライトのクエリをこのアプリのものに差し替える。
///
/// 部品のクエリは打ち消し線・タスクの記法・引用の本文などを拾わず、記法の記号に名前を付けないため、
/// 文法はそのままにクエリだけを替える。
pub fn register_languages() {
    let registry = LanguageRegistry::singleton();
    for (name, highlights, injections) in [
        (LANGUAGE, BLOCK_HIGHLIGHTS, BLOCK_INJECTIONS),
        (INLINE_LANGUAGE, INLINE_HIGHLIGHTS, ""),
    ] {
        let Some(mut config) = registry.language(name) else {
            // 機能フラグで文法を有効にしているため、ここには来ない。
            continue;
        };
        config.highlights = highlights.into();
        config.injections = injections.into();
        registry.register(name, &config);
    }
}

/// ライトとダークのテーマのハイライトに、Markdownの記法の見た目を入れる。
///
/// テーマを切り替えるとハイライトはテーマの設定から読み直されるため、
/// 今のテーマだけでなく、ライトとダークの設定そのものを書き換える。
pub fn apply_to_theme(cx: &mut App) {
    Theme::update(cx, |theme| {
        theme.light_theme = with_markdown_syntax(&theme.light_theme, &ThemeColor::light());
        theme.dark_theme = with_markdown_syntax(&theme.dark_theme, &ThemeColor::dark());
    });
    let mode = Theme::global(cx).mode;
    Theme::change(mode, None, cx);
}

fn with_markdown_syntax(config: &Rc<ThemeConfig>, colors: &ThemeColor) -> Rc<ThemeConfig> {
    let mut config = (**config).clone();
    let mut highlight = config.highlight.clone().unwrap_or_default();
    highlight.syntax = markdown_syntax(&highlight.syntax, colors);
    config.highlight = Some(highlight);
    Rc::new(config)
}

/// クエリで使うハイライトの名前ごとの見た目を、テーマの色から作る。
///
/// どの記法を装飾するかは、今のWikinoのエディタを基準にする。
/// 記法の記号と引用は本文より薄い色にし、見出し・太字・リンク・リンク先・コードはそれぞれの色にする。
/// 記号は、見出し・太字・リンクの中にあっても薄い色にする。
/// 太字は太く、斜体は斜めにする。
/// 色の値はWikinoに合わせず、ライトとダークのテーマの色から選ぶ。
/// クエリで使わない名前は、元のテーマの見た目のまま残す。
fn markdown_syntax(base: &SyntaxColors, colors: &ThemeColor) -> SyntaxColors {
    let mut syntax = base.clone();
    let muted = Some(style(Some(colors.muted_foreground), Look::Plain));
    syntax.punctuation = muted;
    syntax.comment = muted;
    syntax.title = Some(style(Some(colors.red), Look::Plain));
    // 斜体は色を変えず、見出しや太字の中でもその色のまま斜体にする。
    syntax.emphasis = Some(style(None, Look::Italic));
    syntax.emphasis_strong = Some(style(Some(colors.blue), Look::Bold));
    // テーマのリンクの色は本文とほぼ同じ色のため、リンクには基本色の1つを使う。
    syntax.link_text = Some(style(Some(colors.cyan), Look::Plain));
    syntax.link_uri = Some(style(Some(colors.magenta), Look::Plain));
    let code = Some(style(Some(colors.green), Look::Plain));
    syntax.text_code_span = code;
    syntax.text_literal = code;
    syntax
}

/// 色のほかに変える書体。
#[derive(Clone, Copy)]
enum Look {
    Plain,
    Bold,
    Italic,
}

/// テーマのスタイルを作る。
///
/// スタイルのフィールドは外から設定できず、テーマのファイルから読む前提のため、
/// テーマのファイルと同じ形の値から読み込む。
/// 指定しない項目は含めず、重なった記法 (見出しの中の斜体など) の見た目を上書きしないようにする。
fn style(color: Option<Hsla>, look: Look) -> ThemeStyle {
    let mut value = serde_json::Map::new();
    if let Some(color) = color {
        value.insert("color".to_owned(), serde_json::json!(color));
    }
    match look {
        Look::Plain => {}
        Look::Bold => {
            value.insert("font_weight".to_owned(), serde_json::json!(700));
        }
        Look::Italic => {
            value.insert("font_style".to_owned(), serde_json::json!("italic"));
        }
    }
    serde_json::from_value(serde_json::Value::Object(value))
        .expect("テーマのスタイルの形が部品と合わない")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::highlighter::SyntaxHighlighter;
    use gpui_kit::component::input::{HighlightStyleResolver, Rope};
    use gpui_kit::{FontStyle, FontWeight, HighlightStyle};
    use std::time::{Duration, Instant};

    struct Resolver(SyntaxColors);

    impl HighlightStyleResolver for Resolver {
        fn style(&self, name: &str) -> Option<HighlightStyle> {
            self.0.style(name)
        }
    }

    /// 部品が、段落や見出しの中の記法 (注入した言語) の解析にかける時間の上限。
    /// これを超えると、部品はその記法の装飾を捨てる。
    const INJECTION_PARSE_LIMIT: Duration = Duration::from_millis(20);

    /// 見本の本文を装飾し、範囲ごとの見た目を返す。
    ///
    /// 開発コンテナでは、まれに解析が一時的に止まり、時間の上限を超えて段落の中の装飾が抜ける。
    /// テストが装飾の中身を確かめられるよう、解析が上限より長くかかったときは装飾し直す。
    fn highlight(text: &str, colors: &ThemeColor) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
        register_languages();
        let rope = Rope::from_str(text);
        let mut attempt = 1;
        let highlighter = loop {
            let mut highlighter = SyntaxHighlighter::new(LANGUAGE);
            let started = Instant::now();
            highlighter.update(None, &rope, None);
            if started.elapsed() < INJECTION_PARSE_LIMIT || attempt == 5 {
                break highlighter;
            }
            attempt += 1;
        };
        assert_eq!(
            highlighter.language().as_ref(),
            LANGUAGE,
            "クエリを読めず、装飾の無い言語に替わった"
        );
        let resolver = Resolver(markdown_syntax(&SyntaxColors::default(), colors));
        highlighter.styles(&(0..text.len()), &resolver)
    }

    /// 本文の中で最初に見つかった `needle` の範囲全体に付いた見た目を返す。
    ///
    /// 範囲の中で見た目が分かれていれば、その旨で失敗する。
    fn style_of(text: &str, needle: &str, colors: &ThemeColor) -> HighlightStyle {
        let start = text
            .find(needle)
            .unwrap_or_else(|| panic!("見本に {needle:?} が無い"));
        let end = start + needle.len();
        let styles = highlight(text, colors);
        let covering: Vec<_> = styles
            .iter()
            .filter(|(range, _)| range.start < end && start < range.end)
            .collect();
        let Some((first_range, first_style)) = covering.first() else {
            return HighlightStyle::default();
        };
        assert!(
            covering.iter().all(|(_, style)| style == first_style)
                && first_range.start <= start
                && covering.last().is_some_and(|(range, _)| end <= range.end),
            "{needle:?} の範囲で見た目が分かれている: {covering:?}"
        );
        *first_style
    }

    fn color_of(text: &str, needle: &str, colors: &ThemeColor) -> Option<Hsla> {
        style_of(text, needle, colors).color
    }

    /// テーマのスタイルを通した色。テーマのスタイルは色を8ビットのRGBで持つため、元の色と少しずれる。
    fn themed(color: Hsla) -> Option<Hsla> {
        HighlightStyle::from(style(Some(color), Look::Plain)).color
    }

    const SAMPLE: &str = "\
# 見出しの文

本文の**日本語の太字**と*斜体*と~~打ち消し線~~と`コード`。

[リンクの文字](https://example.com/page) と <https://example.com/auto> と <user@example.com>

> 引用の文

- 箇条書き
+ 別の記号の箇条書き
1. 番号
1) 括弧の番号
- [ ] まだのタスク
- [x] 済んだタスク

---

<!-- コメント -->

```rust
fn main() {}
```
";

    #[test]
    fn 見出しは見出しの色にし_記号は薄い色にする() {
        for colors in [ThemeColor::light(), ThemeColor::dark()] {
            assert_eq!(
                color_of(SAMPLE, "見出しの文", &colors),
                themed(colors.red),
                "見出しの色が違う"
            );
            assert_eq!(
                color_of(SAMPLE, "#", &colors),
                themed(colors.muted_foreground),
                "見出しの記号が薄い色でない"
            );
        }
    }

    #[test]
    fn setextの見出しは見出しの色にし_下線は薄い色にする() {
        for colors in [ThemeColor::light(), ThemeColor::dark()] {
            for (text, underline) in [("見出しの文\n===\n", "==="), ("見出しの文\n---\n", "---")]
            {
                assert_eq!(
                    color_of(text, "見出しの文", &colors),
                    themed(colors.red),
                    "{text:?} の見出しの色が違う"
                );
                assert_eq!(
                    color_of(text, underline, &colors),
                    themed(colors.muted_foreground),
                    "{text:?} の下線が薄い色でない"
                );
            }
        }
    }

    #[test]
    fn 日本語の太字は太字と太字の色にする() {
        let colors = ThemeColor::light();
        let strong = style_of(SAMPLE, "日本語の太字", &colors);
        assert_eq!(
            strong.font_weight,
            Some(FontWeight::BOLD),
            "太字になっていない"
        );
        assert_eq!(strong.color, themed(colors.blue), "太字の色が違う");
        assert_eq!(
            color_of(SAMPLE, "**", &colors),
            themed(colors.muted_foreground),
            "太字の記号が薄い色でない"
        );
    }

    #[test]
    fn 斜体は斜体にする() {
        let colors = ThemeColor::light();
        assert_eq!(
            style_of(SAMPLE, "斜体", &colors).font_style,
            Some(FontStyle::Italic),
            "斜体になっていない"
        );
    }

    #[test]
    fn 打ち消し線は記号だけを薄い色にする() {
        // テーマのスタイルに打ち消し線を表す項目が無いため、本文には線を引けない。
        let colors = ThemeColor::light();
        assert_eq!(
            color_of(SAMPLE, "~~", &colors),
            themed(colors.muted_foreground),
            "打ち消し線の記号が薄い色でない"
        );
        assert_eq!(
            style_of(SAMPLE, "打ち消し線", &colors).strikethrough,
            None,
            "打ち消し線を引けないはずの本文に線がある"
        );
    }

    #[test]
    fn リンクのテキストとリンク先は別の色にする() {
        let colors = ThemeColor::light();
        assert_eq!(
            color_of(SAMPLE, "リンクの文字", &colors),
            themed(colors.cyan),
            "リンクのテキストの色が違う"
        );
        assert_eq!(
            color_of(SAMPLE, "https://example.com/page", &colors),
            themed(colors.magenta),
            "リンク先の色が違う"
        );
        assert_eq!(
            color_of(SAMPLE, "<https://example.com/auto>", &colors),
            themed(colors.cyan),
            "URLのリンクの色が違う"
        );
        assert_eq!(
            color_of(SAMPLE, "<user@example.com>", &colors),
            themed(colors.cyan),
            "メールアドレスのリンクの色が違う"
        );
        assert_eq!(
            color_of(SAMPLE, "](", &colors),
            themed(colors.muted_foreground),
            "リンクの括弧が薄い色でない"
        );
    }

    #[test]
    fn リンクと画像の各形式で記号とラベルとリンク先の色を分ける() {
        for colors in [ThemeColor::light(), ThemeColor::dark()] {
            for text in [
                "[通常](https://example.com)\n",
                "![画像](https://example.com)\n",
                "[完全][参照]\n\n[参照]: https://example.com\n",
                "[省略][]\n\n[省略]: https://example.com\n",
                "[短縮]\n\n[短縮]: https://example.com\n",
                "[定義]: https://example.com\n",
            ] {
                let styles = highlight(text, &colors);
                for (offset, ch) in text.char_indices() {
                    let expected = match ch {
                        '!' | '[' | ']' | '(' | ')' => themed(colors.muted_foreground),
                        ':' if text[..offset].ends_with(']') => themed(colors.muted_foreground),
                        _ if !ch.is_ascii() => themed(colors.cyan),
                        _ => continue,
                    };
                    let (_, actual) = styles
                        .iter()
                        .find(|(range, _)| range.contains(&offset))
                        .expect("文字の装飾が無い");
                    assert_eq!(
                        actual.color, expected,
                        "{text:?} の {offset} バイト目 ({ch:?}) の色が違う"
                    );
                }
                assert_eq!(
                    color_of(text, "https://example.com", &colors),
                    themed(colors.magenta),
                    "{text:?} のリンク先の色が違う"
                );
            }
        }
    }

    #[test]
    fn コードはコードの色にし_コードブロックの中を言語ごとに色付けしない() {
        let colors = ThemeColor::light();
        assert_eq!(
            color_of(SAMPLE, "コード", &colors),
            themed(colors.green),
            "インラインのコードの色が違う"
        );
        assert_eq!(
            color_of(SAMPLE, "`", &colors),
            themed(colors.muted_foreground),
            "インラインのコードの記号が薄い色でない"
        );
        assert_eq!(
            color_of(SAMPLE, "fn main() {}", &colors),
            themed(colors.green),
            "コードブロックの中がコードの色の1色になっていない"
        );
    }

    #[test]
    fn 引用とコメントと記法の記号は本文より薄い色にする() {
        let colors = ThemeColor::light();
        for needle in [
            "引用の文",
            "<!-- コメント -->",
            "- ",
            "+ ",
            "1.",
            "1)",
            "[ ]",
            "[x]",
            "---",
        ] {
            assert_eq!(
                color_of(SAMPLE, needle, &colors),
                themed(colors.muted_foreground),
                "{needle:?} が薄い色でない"
            );
        }
    }

    #[test]
    fn 記法の無い本文は装飾しない() {
        let colors = ThemeColor::light();
        assert_eq!(
            style_of(SAMPLE, "本文の", &colors),
            HighlightStyle::default(),
            "記法の無い本文が装飾された"
        );
    }
}
