//! 四种确定性波形（qa-perf.md §1.2）：`sine` / `random_walk` / `spike` / `step`。
//!
//! 波形按 seed 指派（`seed % 4` 决定波形池的轮转起点），同一 metric 的全部采样
//! 由同一 RNG 子流驱动（`seed ^ PER_METRIC_TWEAK * (m + 1)`），保证同 seed 输出
//! 逐字节可复现。

/// 分体式确定性 PRNG（SplitMix64）：不依赖任何平台随机源。
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// 下一个 u64。
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `[0, 1)` 均匀浮点。
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// `[lo, hi]` 闭区间整数。
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        debug_assert!(lo <= hi);
        if lo == hi {
            return lo;
        }
        let span = (hi - lo + 1) as u64;
        lo + (self.next_u64() % span) as i64
    }
}

/// 波形种类（按 seed 指派）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waveform {
    Sine,
    RandomWalk,
    Spike,
    Step,
}

/// 按 `(seed, metric_index)` 确定性指派波形。
pub fn assign_waveform(seed: u64, metric_index: usize) -> Waveform {
    let pool = [
        Waveform::Sine,
        Waveform::RandomWalk,
        Waveform::Spike,
        Waveform::Step,
    ];
    pool[((seed as usize) % 4 + metric_index) % 4]
}

/// 一个 metric 的取值域（数值域与小数位决定行宽，行宽参与体积标定）。
#[derive(Debug, Clone, Copy)]
pub struct MetricDomain {
    pub min: f64,
    pub max: f64,
    pub decimals: usize,
}

impl MetricDomain {
    pub fn new(min: f64, max: f64, decimals: usize) -> Self {
        Self { min, max, decimals }
    }
}

/// 波形采样器：同一种子流逐行产出 `[min, max]` 内的有限浮点。
#[derive(Debug, Clone)]
pub struct WaveformSampler {
    kind: Waveform,
    rng: Rng,
    min: f64,
    max: f64,
    span: f64,
    /// 状态：random_walk 当前值；step 当前档位；spike 当前基线。
    state: f64,
    /// spike：距下一次突峰的行数倒计时。
    spike_timer: u64,
}

impl WaveformSampler {
    /// 以 metric 专属种子构造采样器（min/max/decimals 由 `domain` 给出）。
    pub fn new(seed: u64, metric_index: usize, domain: MetricDomain) -> Self {
        let kind = assign_waveform(seed, metric_index);
        let mut rng = Rng::new(metric_substream(seed, metric_index));
        let span = domain.max - domain.min;
        let state = domain.min + rng.next_f64() * span;
        let spike_timer = match kind {
            Waveform::Spike => 1 + (rng.next_f64() * 200.0) as u64,
            _ => 0,
        };
        Self {
            kind,
            rng,
            min: domain.min,
            max: domain.max,
            span,
            state,
            spike_timer,
        }
    }

    /// 第 `row` 行的采样值（row 仅用于 sine 周期推算，非随机源）。
    pub fn next(&mut self, row: u64) -> f64 {
        let v = match self.kind {
            Waveform::Sine => {
                // 周期 30~300s，行级时间步长 100ms → 300~3000 行每周期。
                let period = 300u64 + (self.rng.next_u64() % 2701u64);
                let phase = 2.0 * std::f64::consts::PI * (row as f64) / (period as f64);
                let mid = self.min + self.span * 0.5;
                let amp = self.span * (0.3 + self.rng.next_f64() * 0.3);
                mid + amp * phase.sin()
            }
            Waveform::RandomWalk => {
                let step = self.span * 0.02;
                let mut v = self.state + (self.rng.next_f64() * 2.0 - 1.0) * step;
                if v < self.min {
                    v = self.min + (self.min - v).min(self.span * 0.05);
                }
                if v > self.max {
                    v = self.max - (v - self.max).min(self.span * 0.05);
                }
                self.state = v;
                v
            }
            Waveform::Spike => {
                if self.spike_timer == 0 {
                    // 突峰：瞬时冲高后回落。
                    self.spike_timer = 1 + (self.rng.next_f64() * 300.0) as u64;
                    self.state = self.max - self.span * (0.05 + self.rng.next_f64() * 0.05);
                    self.state
                } else {
                    self.spike_timer -= 1;
                    // 缓降回基线。
                    self.state = (self.state - self.span * 0.01).max(self.min);
                    self.state
                }
            }
            Waveform::Step => {
                // 每 50~200 行跳变一次到新档位。
                if self.rng.next_f64() < 0.008 {
                    self.state = self.min + self.rng.next_f64() * self.span;
                }
                self.state
            }
        };
        v.clamp(self.min, self.max)
    }
}

/// metric 专属 RNG 子流（同 seed 跨平台一致）。
fn metric_substream(seed: u64, metric_index: usize) -> u64 {
    seed ^ 0xD1B5_4A32_D192_ED03u64.wrapping_mul((metric_index as u64).wrapping_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_across_instances() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn waveform_values_stay_in_domain_and_finite() {
        for kind in [Waveform::Sine, Waveform::RandomWalk, Waveform::Spike, Waveform::Step] {
            // 强制四种波形各测一遍：metric 索引从 0..4 覆盖全部指派。
            for m in 0..4 {
                let mut s = WaveformSampler::new(7, m, MetricDomain::new(0.0, 120.0, 2));
                for row in 0..5000u64 {
                    let v = s.next(row);
                    assert!(v.is_finite(), "{kind:?} m={m} row={row}");
                    assert!((0.0..=120.0).contains(&v), "{kind:?} m={m} v={v}");
                }
            }
        }
    }

    #[test]
    fn waveform_assignment_rotates_with_seed() {
        let w0 = assign_waveform(42, 0);
        let w1 = assign_waveform(42, 1);
        assert_ne!(w0, w1, "相邻 metric 的波形应不同（池轮转）");
        assert_eq!(assign_waveform(42, 4), w0, "周期 4 轮转");
    }

    #[test]
    fn range_respects_bounds() {
        let mut r = Rng::new(99);
        for _ in 0..10000 {
            let v = r.range(-5, 5);
            assert!((-5..=5).contains(&v));
        }
    }
}
