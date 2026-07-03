//! The recipe: the kernel's only work primitive.
//!
//! `c_0·A_0 + c_1·A_1 + ... -> p_0·B_0 + p_1·B_1 + ...  after d ticks`
//!
//! In counting semantics the number of completed firings by tick `t` is
//! `k(t) = min_i floor(N_i(t - d) / c_i)` and output leg `j` carries
//! `p_j * k(t)`. Everything here is closed over ultimately periodic maps, so
//! a feedforward net of recipes summarizes exactly.

use crate::counting::Counting;

/// A recipe with positional input/output legs. Item typing lives a layer up
/// (the wiring term language, M1); the kernel algebra is per-leg.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Recipe {
    /// Units consumed per firing on each input leg (each >= 1).
    pub consume: Vec<u64>,
    /// Units produced per firing on each output leg.
    pub produce: Vec<u64>,
    /// Ticks from consuming to producing.
    pub latency: u64,
}

impl Recipe {
    pub fn new(consume: Vec<u64>, produce: Vec<u64>, latency: u64) -> Self {
        assert!(!consume.is_empty(), "recipe needs at least one input leg");
        assert!(consume.iter().all(|&c| c >= 1), "consume ratios must be >= 1");
        Recipe { consume, produce, latency }
    }

    /// Completed firings by tick `t`, as a counting map (before latency).
    pub fn firings(&self, inputs: &[Counting]) -> Counting {
        assert_eq!(inputs.len(), self.consume.len(), "input leg count mismatch");
        let mut acc: Option<Counting> = None;
        for (n, &c) in inputs.iter().zip(&self.consume) {
            let leg = n.scale_floor(1, c);
            acc = Some(match acc {
                None => leg,
                Some(a) => a.min(&leg),
            });
        }
        acc.unwrap()
    }

    /// Output counting maps, one per output leg.
    pub fn apply(&self, inputs: &[Counting]) -> Vec<Counting> {
        let fired = self.firings(inputs).shift(self.latency);
        self.produce.iter().map(|&p| fired.scale_floor(p, 1)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counting::test_support::{random_counting, Rng};

    const WINDOW: u64 = 300;

    #[test]
    fn apply_matches_direct_formula() {
        let mut rng = Rng(0x004e_c19e_5a11_dab4);
        for _ in 0..200 {
            let legs = rng.below(3) as usize + 1;
            let consume: Vec<u64> = (0..legs).map(|_| rng.below(4) + 1).collect();
            let produce: Vec<u64> = (0..legs).map(|_| rng.below(4)).collect();
            let latency = rng.below(8);
            let inputs: Vec<Counting> = (0..legs).map(|_| random_counting(&mut rng)).collect();
            let r = Recipe::new(consume.clone(), produce.clone(), latency);
            let outs = r.apply(&inputs);
            for t in 0..WINDOW {
                let k = if t < latency {
                    0
                } else {
                    inputs
                        .iter()
                        .zip(&consume)
                        .map(|(n, &c)| n.eval(t - latency) / c)
                        .min()
                        .unwrap()
                };
                for (j, out) in outs.iter().enumerate() {
                    assert_eq!(out.eval(t), produce[j] * k, "t={t} j={j} r={r:?}");
                }
            }
        }
    }

    #[test]
    fn round_robin_splitter() {
        // 2·A -> 1·L + 1·R on a full belt: each side gets every other item.
        let split = Recipe::new(vec![2], vec![1, 1], 0);
        let outs = split.apply(&[Counting::unit_rate()]);
        assert_eq!(outs[0], outs[1]);
        assert_eq!(outs[0].rate(), (1, 2));
        // Conservation in the long run: rates sum to the input rate.
        let (n0, d0) = outs[0].rate();
        let (n1, d1) = outs[1].rate();
        assert_eq!(n0 * d1 + n1 * d0, d0 * d1);
    }

    #[test]
    fn assembler_chain() {
        // gears: 2·iron -> 1·gear (latency 3); belts feed 1 iron/tick.
        let gears = Recipe::new(vec![2], vec![1], 3);
        let gear_flow = gears.apply(&[Counting::unit_rate()]).pop().unwrap();
        assert_eq!(gear_flow.rate(), (1, 2));
        // science: 1·gear + 1·copper -> 1·pack; copper faster than gears,
        // so gears are the bottleneck.
        let science = Recipe::new(vec![1, 1], vec![1], 5);
        let copper = Counting::unit_rate();
        let pack = science.apply(&[gear_flow, copper]).pop().unwrap();
        assert_eq!(pack.rate(), (1, 2));
        // Latencies stack: first pack completes at t = 3 + 2 + 5 = 10.
        assert_eq!(pack.eval(9), 0);
        assert!(pack.eval(10) >= 1);
    }

    #[test]
    fn sensor_is_consume_and_refund() {
        // K·A -> K·A + pulse with K = 4: pulse leg fires floor(N/4) times.
        // (True refund needs feedback (M2); here we check the leg algebra.)
        let sensor = Recipe::new(vec![4], vec![4, 1], 0);
        let outs = sensor.apply(&[Counting::unit_rate()]);
        assert_eq!(outs[0].rate(), (1, 1)); // items pass through at full rate
        assert_eq!(outs[1].rate(), (1, 4)); // pulses at quarter rate
    }
}
