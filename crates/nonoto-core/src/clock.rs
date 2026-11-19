//! 今の日時を返す時計。

use std::fmt;

use jiff::Timestamp;

/// 今の日時を返す。
///
/// ページの作成と更新の日時を決めるのに使う。
/// テストでは、決まった日時を返す時計を渡す。
pub trait Clock: fmt::Debug {
    /// 今の日時を返す。
    fn now(&self) -> Timestamp;
}

/// システムの時計。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}
