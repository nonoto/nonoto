//! 計測に使う、日本語のMarkdownを空の状態から入力の列で作るデータ。
//!
//! 入力の列は、固定した種の乱数で作り、実行のたびに同じ内容になる。
//! 1文字ずつの入力・IMEの確定に当たる数文字の挿入・打ち直しの削除・貼り付けに当たる大きな挿入を混ぜる。

use std::fmt;

use crate::rng::SeededRng;

/// 本文への編集の1件。位置と長さは、UTF-8のバイト数で数える。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Insert { pos: usize, text: String },
    Delete { pos: usize, len: usize },
}

/// 入力の列と、それを当てたあとの本文。
///
/// デバッグ表示では本文を伏せ、入力の件数と本文の長さを表示する。
pub struct Dataset {
    pub edits: Vec<Edit>,
    pub text: String,
}

impl fmt::Debug for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dataset")
            .field("edits", &self.edits.len())
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

impl Dataset {
    pub fn lines(&self) -> usize {
        self.text.matches('\n').count()
    }
}

const NOUNS: &[&str] = &[
    "会議",
    "議事録",
    "設計",
    "同期",
    "暗号",
    "端末",
    "ノート",
    "編集",
    "版",
    "提案",
    "日本語",
    "入力",
    "変換",
    "見出し",
    "本文",
    "共有",
    "検索",
    "履歴",
    "鍵",
    "保存",
];
const VERBS: &[&str] = &[
    "確かめる",
    "まとめる",
    "書き直す",
    "共有する",
    "比べる",
    "決める",
    "残す",
    "見直す",
];
/// Wikiリンクの題名。`[[` と `]]` は記号として1文字ずつ打ち、題名はIMEで確定する。
const LINK_TITLES: &[&str] = &["週次の会議", "設計の方針", "用語集", "同期の仕組み"];
const EMOJI: &[&str] = &["😀", "👍", "🎉", "👨\u{200D}👩\u{200D}👧", "🇯🇵"];

/// 1行のMarkdownを、Markdownの記号 (1文字ずつ打つ) と、日本語のかたまり (IMEで確定する) に分けて作る。
fn line(rng: &mut SeededRng) -> Vec<Token> {
    let mut tokens = Vec::new();
    match rng.below(10) {
        0 => {
            tokens.push(Token::Ascii("## ".to_owned()));
            tokens.push(Token::Ime(format!(
                "{}の{}",
                rng.pick(NOUNS),
                rng.pick(NOUNS)
            )));
            return tokens;
        }
        1 => return tokens,
        2..=4 => tokens.push(Token::Ascii("- ".to_owned())),
        _ => {}
    }
    for _ in 0..=rng.below(3) {
        tokens.push(Token::Ime(format!("{}の", rng.pick(NOUNS))));
        tokens.push(Token::Ime(format!("{}を", rng.pick(NOUNS))));
        match rng.below(8) {
            0 => {
                tokens.push(Token::Ascii("[[".to_owned()));
                tokens.push(Token::Ime(rng.pick(LINK_TITLES).to_string()));
                tokens.push(Token::Ascii("]]".to_owned()));
            }
            1 => tokens.push(Token::Emoji(rng.pick(EMOJI).to_string())),
            _ => {}
        }
        tokens.push(Token::Ime(format!("{}。", rng.pick(VERBS))));
    }
    tokens
}

enum Token {
    /// 1文字ずつ打つ文字列。
    Ascii(String),
    /// IMEの変換で、まとめて確定する文字列。
    Ime(String),
    /// 絵文字の候補から入れる1文字。
    Emoji(String),
}

struct Typist {
    text: String,
    edits: Vec<Edit>,
}

impl Typist {
    fn insert(&mut self, pos: usize, text: &str) {
        self.text.insert_str(pos, text);
        self.edits.push(Edit::Insert {
            pos,
            text: text.to_owned(),
        });
    }

    fn delete(&mut self, pos: usize, len: usize) {
        self.text.replace_range(pos..pos + len, "");
        self.edits.push(Edit::Delete { pos, len });
    }

    /// 入力の位置を選ぶ。多くは末尾で、ときどき行の頭に戻る。
    fn cursor(&self, rng: &mut SeededRng) -> usize {
        if rng.below(5) != 0 {
            return self.text.len();
        }
        let starts: Vec<usize> = std::iter::once(0)
            .chain(self.text.match_indices('\n').map(|(i, _)| i + 1))
            .filter(|&i| i <= self.text.len())
            .collect();
        *rng.pick(&starts)
    }

    /// 1行を打つ。日本語は数文字ずつ確定し、ときどき打ち直す。
    fn type_line(&mut self, rng: &mut SeededRng, mut pos: usize) {
        for token in line(rng) {
            match token {
                Token::Ascii(text) => {
                    for ch in text.chars() {
                        let s = ch.to_string();
                        self.insert(pos, &s);
                        pos += s.len();
                    }
                }
                Token::Emoji(text) => {
                    self.insert(pos, &text);
                    pos += text.len();
                }
                Token::Ime(text) => {
                    let chars: Vec<char> = text.chars().collect();
                    let mut i = 0;
                    while i < chars.len() {
                        let n = (2 + rng.below(5)).min(chars.len() - i);
                        let chunk: String = chars[i..i + n].iter().collect();
                        self.insert(pos, &chunk);
                        pos += chunk.len();
                        // 打ち直し: 確定した末尾の1から3文字を1文字ずつ消して、まとめて打ち直す。
                        if rng.below(10) == 0 {
                            let back = (1 + rng.below(3)).min(n);
                            let retype: String = chars[i + n - back..i + n].iter().collect();
                            for ch in retype.chars().rev() {
                                pos -= ch.len_utf8();
                                self.delete(pos, ch.len_utf8());
                            }
                            self.insert(pos, &retype);
                            pos += retype.len();
                        }
                        i += n;
                    }
                }
            }
        }
        self.insert(pos, "\n");
    }
}

/// `lines` 行の日本語のMarkdownを、入力の列で作る。
pub fn japanese_markdown(lines: usize, seed: u64) -> Dataset {
    let mut rng = SeededRng::new(seed);
    let mut typist = Typist {
        text: String::new(),
        edits: Vec::new(),
    };
    let mut count = 0;
    while count < lines {
        let pos = typist.cursor(&mut rng);
        if rng.below(20) == 0 {
            // 貼り付け: 数十行をまとめて挿入する。
            let n = (10 + rng.below(50)).min(lines - count);
            let mut block = String::new();
            for _ in 0..n {
                for token in line(&mut rng) {
                    match token {
                        Token::Ascii(s) | Token::Ime(s) | Token::Emoji(s) => block.push_str(&s),
                    }
                }
                block.push('\n');
            }
            typist.insert(pos, &block);
            count += n;
        } else {
            typist.type_line(&mut rng, pos);
            count += 1;
        }
    }
    Dataset {
        edits: typist.edits,
        text: typist.text,
    }
}
