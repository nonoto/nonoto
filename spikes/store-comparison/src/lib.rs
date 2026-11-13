//! 端末上の暗号化ストアの方式を比べるための実装。
//!
//! 方式A (データベース全体をSQLCipherで暗号化する) は `nonoto-core` の `SqlCipherStore` を使う。
//! ここには方式B (素のSQLiteに、レコード単位で暗号化した本文を保存する) と、計測に使う部品を置く。

pub mod index;
pub mod record;
