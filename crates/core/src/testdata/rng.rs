//! 可复现的伪随机数:SplitMix64。
//!
//! 不用 rand crate:同一个种子在不同版本的 Datum 上要生成同样的数据(复现问题、写进测试),
//! 而 rand 不承诺跨版本的取值序列稳定。SplitMix64 只有几行,统计质量对造测试数据绰绰有余。

#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// 由总种子与一个标签(列名)派生独立的子序列:改一列的生成方式不影响其他列。
    pub fn derive(seed: u64, label: &str) -> Self {
        // FNV-1a 混入标签,再过一遍 SplitMix 打散
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in label.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        let mut r = Rng(seed ^ h);
        r.next_u64();
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// [0, 1) 的浮点数。
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// [0, n) 的整数;n 为 0 时返回 0。
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        // 拒绝采样去掉取模偏差
        let zone = u64::MAX - (u64::MAX % n);
        loop {
            let v = self.next_u64();
            if v < zone {
                return v % n;
            }
        }
    }

    /// 闭区间 [lo, hi] 的整数(lo > hi 时交换)。
    pub fn range_i64(&mut self, lo: i64, hi: i64) -> i64 {
        let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        let span = (hi as i128 - lo as i128) as u128;
        if span >= u64::MAX as u128 {
            return self.next_u64() as i64;
        }
        (lo as i128 + self.below(span as u64 + 1) as i128) as i64
    }

    pub fn chance(&mut self, p: f64) -> bool {
        p > 0.0 && self.f64() < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next_u64() & 0xff) as u8).collect()
    }
}

/// 种子的上限(不含):界面要显示、复制种子,且种子要经 nsh 从 JS 传回原生层。nsh 只把绝对值
/// 小于 9×10^15 的整数当整数传(更大的变成浮点,反序列化成 u64 会失败),比 JS 安全整数 2^53 还小一点。
pub const SEED_LIMIT: u64 = 9_000_000_000_000_000;

/// 随机种子(界面默认值):当前时间与地址混一下,不需要密码学强度。小于 [`SEED_LIMIT`]。
pub fn random_seed() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let local = 0u8;
    let mut r = Rng(t ^ (&local as *const u8 as u64).rotate_left(32));
    r.next_u64() % SEED_LIMIT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_seed_stays_below_limit() {
        for _ in 0..10_000 {
            assert!(random_seed() < SEED_LIMIT);
        }
    }

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let xs: Vec<u64> = (0..5).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..5).map(|_| b.next_u64()).collect();
        assert_eq!(xs, ys);
        // 固定的取值序列:换实现会让老种子造出的数据变样,这里钉住
        assert_eq!(Rng::new(0).next_u64(), 0xE220_A839_7B1D_CDAF);
    }

    #[test]
    fn derived_streams_differ_and_are_stable() {
        let a = Rng::derive(7, "email").next_u64();
        let b = Rng::derive(7, "name").next_u64();
        assert_ne!(a, b);
        assert_eq!(a, Rng::derive(7, "email").next_u64());
    }

    #[test]
    fn ranges_stay_inside() {
        let mut r = Rng::new(1);
        for _ in 0..10_000 {
            let v = r.range_i64(-3, 3);
            assert!((-3..=3).contains(&v));
            let f = r.f64();
            assert!((0.0..1.0).contains(&f));
        }
        assert_eq!(r.range_i64(5, 5), 5);
        let _ = r.range_i64(i64::MIN, i64::MAX);
        assert!(random_seed() < (1u64 << 53));
    }
}
