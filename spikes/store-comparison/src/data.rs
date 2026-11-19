//! 計測に使うページとクエリ。固定した種の乱数で作り、実行のたびに同じ内容になる。
//!
//! 語彙は、漢字の熟語・カタカナ語・英単語を数千語作り、助詞や句読点と交互に並べて本文にする。
//! 実際の文章に近い数のバイグラムが現れるよう、語は出現の頻度に偏りを付けて選ぶ。

use jiff::Timestamp;
use nonoto_core::{Page, PageId};

/// 平文が残っていないかを調べる目印。最初のページのタイトルに入れる。
pub const MARKER: &str = "比較用の目印のタイトル";

/// 検索の計測に使うクエリ。
pub struct Query {
    pub text: &'static str,
    /// 何を確かめるクエリか。
    pub label: &'static str,
    /// 検索の時間の必須の条件の判定に含めるか。1文字のクエリは結果を記録するだけにする。
    pub required: bool,
}

pub const QUERIES: [Query; 7] = [
    Query {
        text: "会",
        label: "1文字",
        required: false,
    },
    Query {
        text: "会議",
        label: "2文字",
        required: true,
    },
    Query {
        text: "東京都",
        label: "3文字",
        required: true,
    },
    Query {
        text: "議事録の確認",
        label: "少数のページだけにある長い語",
        required: true,
    },
    Query {
        text: "ケジュ",
        label: "カタカナ語の途中",
        required: true,
    },
    Query {
        text: "velop",
        label: "英単語の途中",
        required: true,
    },
    Query {
        text: "見つからない語句",
        label: "見つからない語",
        required: true,
    },
];

/// 必ず語彙に入れる語。クエリの「会議」「ケジュ」「velop」が、多くのページに現れるようにする。
const FIXED_WORDS: [&str; 6] = [
    "会議",
    "スケジュール",
    "development",
    "設計",
    "資料",
    "リリース",
];

/// 語と語のあいだに置く助詞・語尾・句読点。
const FUNCTION_WORDS: [&str; 16] = [
    "の",
    "を",
    "が",
    "に",
    "は",
    "で",
    "と",
    "した",
    "する",
    "について",
    "します",
    "、",
    "。",
    "。\n",
    " ",
    "\n\n",
];

const KANJI: &str = "会議設計予定資料共有実装試験確認進行決定課題対応大阪名古屋京都福岡札幌仙台横浜神戸\
    広島新潟静岡熊本鹿児島沖縄北海道本州四国九州関東関西中部東北北陸近畿中国地方都市町村区役所\
    学校病院銀行会社社員部長課長係長担当者顧客利用登録変更削除追加作成更新保存検索表示画面機能\
    要件仕様方針判断結果報告連絡相談調査分析評価改善計画目標期限優先順位予算費用売上利益在庫発注\
    納品請求支払契約交渉提案承認申請手続書類記録管理運用保守障害復旧監視通知警告安全品質性能速度\
    容量時間曜日月年週朝昼夜春夏秋冬天気温度雨雪風晴曇電話番号住所氏名年齢性別家族友人先生生徒";
const KATAKANA: &str = "アイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワン\
    ガギグゲゴザジズゼゾダヂヅデドバビブベボパピプペポャュョッー";
const LATIN: &str = "abcdefghijklmnopqrstuvwxyz";

/// 本文の文字数の範囲。平均がおよそ900文字になる。
const BODY_CHARS: std::ops::Range<usize> = 600..1200;

/// 漢字の熟語・カタカナ語・英単語の数。
const KANJI_WORDS: usize = 2000;
const KATAKANA_WORDS: usize = 800;
const LATIN_WORDS: usize = 800;

/// 固定した種の乱数 (線形合同法)。
struct Random(u64);

impl Random {
    fn next(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as usize
    }

    fn below(&mut self, limit: usize) -> usize {
        self.next() % limit
    }

    /// 0から `limit` 未満の値を、小さい値ほど多く出るように返す。
    fn skewed(&mut self, limit: usize) -> usize {
        let a = self.below(limit);
        let b = self.below(limit);
        a.min(b)
    }
}

fn word(random: &mut Random, alphabet: &[char], length: std::ops::Range<usize>) -> String {
    let count = length.start + random.below(length.end - length.start);
    (0..count)
        .map(|_| alphabet[random.below(alphabet.len())])
        .collect()
}

/// 語彙を作る。先頭ほど多く使われる。
fn vocabulary(random: &mut Random) -> Vec<String> {
    let kanji: Vec<char> = KANJI.chars().filter(|c| !c.is_whitespace()).collect();
    let katakana: Vec<char> = KATAKANA.chars().filter(|c| !c.is_whitespace()).collect();
    let latin: Vec<char> = LATIN.chars().collect();
    let mut words: Vec<String> = FIXED_WORDS.iter().map(|&word| word.to_owned()).collect();
    let mut generated = Vec::with_capacity(KANJI_WORDS + KATAKANA_WORDS + LATIN_WORDS);
    for (alphabet, count, length) in [
        (&kanji, KANJI_WORDS, 2..4),
        (&katakana, KATAKANA_WORDS, 3..7),
        (&latin, LATIN_WORDS, 3..10),
    ] {
        for _ in 0..count {
            generated.push(word(random, alphabet, length.clone()));
        }
    }
    // 種類ごとに固まらないよう、並びを混ぜてから頻度の順位を付ける。
    for i in (1..generated.len()).rev() {
        generated.swap(i, random.below(i + 1));
    }
    words.append(&mut generated);
    words
}

/// 日本語のページを決まった順序で作る。
///
/// 「東京都」は100件に1件、「議事録の確認」は1000件に1件だけに入れ、少数のページだけにある語の検索を計る。
pub fn generate_pages(count: usize) -> Vec<Page> {
    let mut random = Random(0x5eed);
    let words = vocabulary(&mut random);
    let base = Timestamp::from_millisecond(1_700_000_000_000).expect("日時を作れなかった");
    (0..count)
        .map(|i| {
            let target = BODY_CHARS.start + random.below(BODY_CHARS.end - BODY_CHARS.start);
            let mut body = String::new();
            let mut chars = 0;
            while chars < target {
                let content = &words[random.skewed(words.len())];
                let function = FUNCTION_WORDS[random.below(FUNCTION_WORDS.len())];
                chars += content.chars().count() + function.chars().count();
                body.push_str(content);
                body.push_str(function);
            }
            if i % 100 == 0 {
                body.push_str("東京都の拠点で");
            }
            if i % 1000 == 0 {
                body.push_str("議事録の確認をした");
            }
            let title = if i == 0 {
                MARKER.to_owned()
            } else {
                format!("{}の{}", words[random.skewed(words.len())], i)
            };
            let at = base
                .checked_add(jiff::SignedDuration::from_secs(
                    i64::try_from(i).expect("ページの番号はi64に収まる"),
                ))
                .expect("日時を作れなかった");
            Page {
                id: PageId::new(),
                title,
                body,
                created_at: at,
                updated_at: at,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn 同じ件数なら同じ本文とタイトルになる() {
        let contents =
            |pages: Vec<Page>| -> Vec<_> { pages.into_iter().map(|p| (p.title, p.body)).collect() };

        assert_eq!(
            contents(generate_pages(50)),
            contents(generate_pages(50)),
            "実行のたびに内容が変わる"
        );
    }

    #[test]
    fn 本文の平均はおよそ900文字になる() {
        let pages = generate_pages(1_000);
        let average = pages.iter().map(|p| p.body.chars().count()).sum::<usize>() / pages.len();

        assert!(
            (850..=1000).contains(&average),
            "本文の平均の文字数が想定から外れる: {average}"
        );
    }

    #[test]
    fn 語彙は数千語になり種類が混ざる() {
        let words = vocabulary(&mut Random(0x5eed));
        let distinct: HashSet<_> = words.iter().collect();

        assert!(distinct.len() > 3_000, "語彙が少ない: {}", distinct.len());
        assert!(
            words
                .iter()
                .any(|w| w.chars().all(|c| c.is_ascii_lowercase())),
            "英単語が無い"
        );
        assert!(
            words
                .iter()
                .any(|w| w.chars().all(|c| KATAKANA.contains(c)) && w != "スケジュール"),
            "カタカナ語が無い"
        );
    }

    #[test]
    fn 見つからない語以外のクエリはページに現れる() {
        let pages = generate_pages(2_000);
        for query in QUERIES {
            let found = pages
                .iter()
                .filter(|p| p.title.contains(query.text) || p.body.contains(query.text))
                .count();
            if query.text == "見つからない語句" {
                assert_eq!(found, 0, "見つからないはずの語が見つかった");
            } else {
                assert!(found > 0, "クエリが現れない: {}", query.text);
            }
        }
    }

    #[test]
    fn 少数のページだけにある語は決まった割合で入る() {
        let pages = generate_pages(2_000);
        let count = |text: &str| pages.iter().filter(|p| p.body.contains(text)).count();

        assert_eq!(count("議事録の確認"), 2, "1000件に1件になっていない");
        assert!(count("東京都") >= 20, "100件に1件より少ない");
    }
}
