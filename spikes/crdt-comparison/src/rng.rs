//! 計測のデータとテストの操作の列を作る乱数。

/// 固定した種から、実行のたびに同じ列を出す乱数 (SplitMix64)。
#[derive(Debug)]
pub struct SeededRng(u64);

impl SeededRng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `0..n` の値を返す。`n` は1以上にする。
    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % n as u64).expect("n未満の値はusizeに収まる")
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}
