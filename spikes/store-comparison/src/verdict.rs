//! 計測の結果を、ストアの方式に必須の条件と照らす。
//!
//! 条件の上限は、開発コンテナ (CPU 2コア) で1万ページを計った値で判定するために決めたものである。

use std::time::Duration;

pub const SEARCH_LIMIT: Duration = Duration::from_millis(100);
pub const STARTUP_LIMIT: Duration = Duration::from_millis(500);
pub const STARTUP_RSS_LIMIT_KIB: u64 = 50 * 1024;
pub const WRITE_LIMIT: Duration = Duration::from_millis(50);

/// 1つの変種の計測の結果のうち、判定に使う値。
pub struct Measured {
    /// 2文字の語を、全件を走査せずにインデックスで引くか。
    pub indexes_two_chars: bool,
    /// 判定に含めるクエリの検索の時間 (中央値)。
    pub required_searches: Vec<Duration>,
    pub startup: Duration,
    /// 起動で増えたメモリ。測れないOSでは `None`。
    pub startup_rss_kib: Option<u64>,
    pub insert: Duration,
    /// 更新できたページが無いときは `None`。
    pub update: Option<Duration>,
}

/// 条件ごとの判定。値を測れなかった条件は `None` にする。
pub struct Verdict {
    pub conditions: Vec<(&'static str, Option<bool>)>,
}

impl Verdict {
    /// すべての条件を満たしたか。満たさない条件が無く、測れなかった条件があれば `None` を返す。
    pub fn passed(&self) -> Option<bool> {
        let results = self.conditions.iter().map(|(_, result)| *result);
        if results.clone().any(|result| result == Some(false)) {
            Some(false)
        } else if results.clone().any(|result| result.is_none()) {
            None
        } else {
            Some(true)
        }
    }
}

pub fn judge(measured: &Measured) -> Verdict {
    let searches = if measured.required_searches.is_empty() {
        None
    } else {
        Some(
            measured
                .required_searches
                .iter()
                .all(|elapsed| *elapsed <= SEARCH_LIMIT),
        )
    };
    Verdict {
        conditions: vec![
            (
                "2文字の語をインデックスで引く",
                Some(measured.indexes_two_chars),
            ),
            ("検索が100ms以内", searches),
            ("起動が500ms以内", Some(measured.startup <= STARTUP_LIMIT)),
            (
                "起動で増えるメモリが50MiB以内",
                measured
                    .startup_rss_kib
                    .map(|kib| kib <= STARTUP_RSS_LIMIT_KIB),
            ),
            (
                "1件の保存と更新が50ms以内",
                measured
                    .update
                    .map(|update| measured.insert <= WRITE_LIMIT && update <= WRITE_LIMIT),
            ),
        ],
    }
}

/// 判定を表に書く記号。
pub fn mark(result: Option<bool>) -> &'static str {
    match result {
        Some(true) => "満たす",
        Some(false) => "満たさない",
        None => "測れない",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured() -> Measured {
        Measured {
            indexes_two_chars: true,
            required_searches: vec![Duration::from_millis(5), SEARCH_LIMIT],
            startup: STARTUP_LIMIT,
            startup_rss_kib: Some(STARTUP_RSS_LIMIT_KIB),
            insert: WRITE_LIMIT,
            update: Some(WRITE_LIMIT),
        }
    }

    #[test]
    fn 上限ちょうどの値は条件を満たす() {
        assert_eq!(
            judge(&measured()).passed(),
            Some(true),
            "上限ちょうどで満たさない"
        );
    }

    #[test]
    fn 上限を超えた条件だけが満たさないになる() {
        let mut slow = measured();
        slow.required_searches
            .push(SEARCH_LIMIT + Duration::from_nanos(1));
        slow.update = Some(WRITE_LIMIT + Duration::from_nanos(1));

        let verdict = judge(&slow);

        let failed: Vec<_> = verdict
            .conditions
            .iter()
            .filter(|(_, result)| *result == Some(false))
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            failed,
            ["検索が100ms以内", "1件の保存と更新が50ms以内"],
            "満たさない条件が期待と異なる"
        );
        assert_eq!(verdict.passed(), Some(false), "全体の判定が満たすになった");
    }

    #[test]
    fn 二文字をインデックスで引かない変種は満たさない() {
        let mut scan = measured();
        scan.indexes_two_chars = false;

        assert_eq!(
            judge(&scan).passed(),
            Some(false),
            "走査の変種が条件を満たした"
        );
    }

    #[test]
    fn 測れない値があれば全体の判定も測れないになる() {
        let mut unknown = measured();
        unknown.startup_rss_kib = None;

        assert_eq!(
            judge(&unknown).passed(),
            None,
            "測れない値があるのに判定した"
        );
    }

    #[test]
    fn 満たさない条件があれば測れない値があっても満たさないになる() {
        let mut both = measured();
        both.startup_rss_kib = None;
        both.indexes_two_chars = false;

        assert_eq!(
            judge(&both).passed(),
            Some(false),
            "満たさない条件があるのに満たさないにならない"
        );
    }
}
