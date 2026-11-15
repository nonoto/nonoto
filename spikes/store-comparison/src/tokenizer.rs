//! FTS5に登録する、文字を細かく分けるトークナイザーの分割の規則。
//!
//! 文字の種類 (漢字・かな・カタカナ・英数字・記号・空白) を問わず、すべての文字を同じ規則で分け、
//! 文字の正規化もしない。FTS5のフレーズの検索が、文字列の部分一致と同じ結果になるようにするためである。

/// トークンへの分け方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tokenizer {
    /// 連続する2文字を1トークンにする。
    Bigram,
    /// 1文字を1トークンにする。
    Unigram,
}

/// 何のためにテキストを分けるか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// インデックスに入れるページのテキスト。
    Document,
    /// 検索のクエリ。
    Query,
}

/// テキストの中のトークン。`start` と `end` はテキストの中のバイトの位置である。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

impl Tokenizer {
    /// FTS5の `tokenize` のオプションに書く名前。
    pub fn name(self) -> &'static str {
        match self {
            Self::Bigram => "bigram",
            Self::Unigram => "unigram",
        }
    }

    /// テキストをトークンに分け、出てきた順に `emit` へ渡す。`emit` がエラーを返したらそこで止める。
    ///
    /// バイグラムでは、ページのテキストの末尾の1文字も1文字のトークンにする。
    /// すべての文字が、ある2文字のトークンの先頭か末尾の1文字のトークンになるため、
    /// 1文字のクエリをトークンの前方一致で引ける。
    /// クエリでは、2文字以上ならバイグラムだけにし、隣り合うトークンのフレーズとして引かせる。
    pub fn tokenize<'a, E>(
        self,
        text: &'a str,
        purpose: Purpose,
        mut emit: impl FnMut(Token<'a>) -> Result<(), E>,
    ) -> Result<(), E> {
        let token = |start: usize, end: usize| Token {
            text: &text[start..end],
            start,
            end,
        };
        let mut chars = text
            .char_indices()
            .map(|(start, c)| (start, start + c.len_utf8()));
        match self {
            Self::Unigram => {
                for (start, end) in chars {
                    emit(token(start, end))?;
                }
            }
            Self::Bigram => {
                let Some(mut previous) = chars.next() else {
                    return Ok(());
                };
                let mut single = true;
                for current in chars {
                    emit(token(previous.0, current.1))?;
                    previous = current;
                    single = false;
                }
                if purpose == Purpose::Document || single {
                    emit(token(previous.0, previous.1))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::*;

    fn tokens(tokenizer: Tokenizer, text: &str, purpose: Purpose) -> Vec<(&str, usize, usize)> {
        let mut tokens = Vec::new();
        tokenizer
            .tokenize(text, purpose, |token| {
                tokens.push((token.text, token.start, token.end));
                Ok::<_, Infallible>(())
            })
            .expect("トークンに分けられなかった");
        tokens
    }

    #[test]
    fn バイグラムはページの末尾の1文字もトークンにする() {
        assert_eq!(
            tokens(Tokenizer::Bigram, "東京都", Purpose::Document),
            [("東京", 0, 6), ("京都", 3, 9), ("都", 6, 9)],
            "トークンと位置が期待と異なる"
        );
    }

    #[test]
    fn バイグラムは2文字以上のクエリでは末尾の1文字をトークンにしない() {
        assert_eq!(
            tokens(Tokenizer::Bigram, "東京都", Purpose::Query),
            [("東京", 0, 6), ("京都", 3, 9)],
            "トークンと位置が期待と異なる"
        );
    }

    #[test]
    fn バイグラムは1文字のテキストを1文字のトークンにする() {
        for purpose in [Purpose::Document, Purpose::Query] {
            assert_eq!(
                tokens(Tokenizer::Bigram, "会", purpose),
                [("会", 0, 3)],
                "トークンと位置が期待と異なる: {purpose:?}"
            );
        }
    }

    #[test]
    fn 文字の種類を問わず同じ規則で分ける() {
        assert_eq!(
            tokens(Tokenizer::Bigram, "aBテ 。", Purpose::Document),
            [
                ("aB", 0, 2),
                ("Bテ", 1, 5),
                ("テ ", 2, 6),
                (" 。", 5, 9),
                ("。", 6, 9)
            ],
            "バイグラムのトークンと位置が期待と異なる"
        );
        assert_eq!(
            tokens(Tokenizer::Unigram, "aBテ 。", Purpose::Document),
            [
                ("a", 0, 1),
                ("B", 1, 2),
                ("テ", 2, 5),
                (" ", 5, 6),
                ("。", 6, 9)
            ],
            "ユニグラムのトークンと位置が期待と異なる"
        );
    }

    #[test]
    fn ユニグラムはページとクエリを同じに分ける() {
        assert_eq!(
            tokens(Tokenizer::Unigram, "議事録", Purpose::Document),
            tokens(Tokenizer::Unigram, "議事録", Purpose::Query),
            "ページとクエリでトークンが異なる"
        );
    }

    #[test]
    fn 空のテキストはトークンにならない() {
        for tokenizer in [Tokenizer::Bigram, Tokenizer::Unigram] {
            for purpose in [Purpose::Document, Purpose::Query] {
                assert!(
                    tokens(tokenizer, "", purpose).is_empty(),
                    "空のテキストからトークンができた: {tokenizer:?} {purpose:?}"
                );
            }
        }
    }

    #[test]
    fn emitがエラーを返したら止める() {
        let mut count = 0;
        let result = Tokenizer::Unigram.tokenize("議事録", Purpose::Document, |_| {
            count += 1;
            Err("止める")
        });

        assert_eq!(result, Err("止める"), "エラーを返さなかった");
        assert_eq!(count, 1, "エラーのあとも続けた");
    }
}
