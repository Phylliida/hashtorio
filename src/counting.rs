//! Ultimately periodic counting functions in canonical form.
//!
//! A [`Counting`] represents a monotone staircase `N : tick -> count`
//! (cumulative items across a wire) that is *ultimately periodic*:
//! there exist `T` (transient length), `p >= 1` (period) and `q` (slope,
//! items per period) such that `N(t + p) = N(t) + q` for all `t >= T`.
//!
//! Every value is kept in canonical form (minimal period, then minimal
//! transient), so structural equality coincides with semantic equality and
//! the derived `Hash` makes a `Counting` directly usable as a cache key.
//!
//! The kernel fragment (recipes + markings + wiring, see DESIGN.md) is closed
//! under the operations here: [`Counting::add`] (merge), [`Counting::min`]
//! (synchronization), [`Counting::scale_floor`] (ratio gearing),
//! [`Counting::shift`] (latency).

/// Monotone, ultimately periodic staircase `N : u64 -> u64`.
///
/// Representation: explicit samples for `t` in `[0, transient + period)`;
/// beyond that, `N(t + period) = N(t) + slope` (anchored at `t >= transient`).
///
/// Invariants (checked by [`Counting::validate`], guaranteed after
/// [`Counting::normalize`]):
/// - `period >= 1` and `samples.len() == transient + period`
/// - `samples` is non-decreasing
/// - seam monotonicity: `samples[transient] + slope >= samples[transient + period - 1]`
/// - canonical: no smaller period, no smaller transient represents the same function
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Counting {
    samples: Vec<u64>,
    transient: usize,
    period: u64,
    slope: u64,
}

pub(crate) fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

fn lcm(a: u64, b: u64) -> u64 {
    a / gcd(a, b) * b
}

fn mul(a: u64, b: u64) -> u64 {
    a.checked_mul(b).expect("count overflow (mul)")
}

fn addu(a: u64, b: u64) -> u64 {
    a.checked_add(b).expect("count overflow (add)")
}

impl Counting {
    /// Build from raw parts and normalize. Panics if the parts do not
    /// describe a monotone staircase.
    pub fn from_parts(samples: Vec<u64>, transient: usize, period: u64, slope: u64) -> Self {
        let c = Counting { samples, transient, period, slope };
        c.check_well_formed();
        c.normalize()
    }

    /// Like [`Counting::from_parts`] but returns `None` instead of panicking,
    /// for callers whose parts are guesses (e.g. the fixed-point summarizer).
    pub fn try_from_parts(
        samples: Vec<u64>,
        transient: usize,
        period: u64,
        slope: u64,
    ) -> Option<Self> {
        let c = Counting { samples, transient, period, slope };
        if c.is_well_formed() {
            Some(c.normalize())
        } else {
            None
        }
    }

    /// The constant-zero function (empty wire).
    pub fn zero() -> Self {
        Counting { samples: vec![0], transient: 0, period: 1, slope: 0 }
    }

    /// The constant function `N(t) = c` (e.g. an initial marking).
    pub fn constant(c: u64) -> Self {
        Counting { samples: vec![c], transient: 0, period: 1, slope: 0 }
    }

    /// One item per tick starting at t = 1: `N(t) = t`.
    /// (`N(0) = 0` because nothing has crossed before time 0.)
    pub fn unit_rate() -> Self {
        Counting { samples: vec![0], transient: 0, period: 1, slope: 1 }
    }

    /// `N(t)` for any `t`.
    pub fn eval(&self, t: u64) -> u64 {
        let window = self.transient as u64 + self.period;
        if t < window {
            self.samples[t as usize]
        } else {
            let k = (t - self.transient as u64) / self.period;
            let r = (t - self.transient as u64) % self.period;
            addu(self.samples[self.transient + r as usize], mul(k, self.slope))
        }
    }

    /// First tick with a nonzero count, or `None` if always zero.
    pub fn first_nonzero(&self) -> Option<u64> {
        if let Some(i) = self.samples.iter().position(|&v| v > 0) {
            return Some(i as u64);
        }
        // Window all zero: the tail first rises one period past the seam.
        if self.slope > 0 {
            Some(self.transient as u64 + self.period)
        } else {
            None
        }
    }

    /// Long-run rate as a reduced rational `(items, ticks)`.
    pub fn rate(&self) -> (u64, u64) {
        let g = gcd(self.slope, self.period);
        if self.slope == 0 {
            (0, 1)
        } else {
            (self.slope / g, self.period / g)
        }
    }

    /// The tail from `t0` onward, re-based to zero: `C'(τ) = C(t0+τ) − C(t0)`.
    /// Exact and closed: the suffix of an ultimately periodic staircase is
    /// ultimately periodic (the transient shrinks by `t0`, floored at zero).
    /// This is half of the relocation-seam algebra (DESIGN-motion.md).
    pub fn suffix(&self, t0: u64) -> Counting {
        let base = self.eval(t0);
        let tail_transient = (self.transient as u64).saturating_sub(t0) as usize;
        let len = tail_transient as u64 + self.period;
        let samples = (0..len).map(|tau| self.eval(t0 + tau) - base).collect();
        Counting { samples, transient: tail_transient, period: self.period, slope: self.slope }
            .normalize()
    }

    /// This staircase up to `ts`, then `tail` re-based on top:
    /// `D(t) = C(t)` for `t < ts`; `D(t) = C(ts) + tail(t−ts)` for `t ≥ ts`.
    /// Exact; the transient is ~`ts` samples — the honest price of replaying
    /// history. Used as module *prehistory* at relocation seams: a module's
    /// state is a function of its input history (Kahn), so continuing it
    /// across a seam means evaluating it on `concat` inputs and taking the
    /// `suffix`. `c.concat(ts, &c.suffix(ts)) == c` for every `c, ts`.
    pub fn concat(&self, ts: u64, tail: &Counting) -> Counting {
        let base = self.eval(ts);
        let t_len = ts as usize + tail.transient;
        let len = t_len as u64 + tail.period;
        let samples = (0..len)
            .map(|t| if t < ts { self.eval(t) } else { addu(base, tail.eval(t - ts)) })
            .collect();
        Counting { samples, transient: t_len, period: tail.period, slope: tail.slope }
            .normalize()
    }

    /// Merge: pointwise sum of counts (deterministic union of two streams).
    pub fn add(&self, other: &Counting) -> Counting {
        let p = lcm(self.period, other.period);
        let t = self.transient.max(other.transient);
        let slope = addu(mul(self.slope, p / self.period), mul(other.slope, p / other.period));
        let samples = (0..t as u64 + p).map(|i| addu(self.eval(i), other.eval(i))).collect();
        Counting { samples, transient: t, period: p, slope }.normalize()
    }

    /// Synchronization: pointwise minimum. Closed on ultimately periodic
    /// staircases: with equal long-run rates the min is periodic on the lcm;
    /// with unequal rates the smaller-rate side eventually wins pointwise and
    /// stays won (its per-lcm-period increment is strictly smaller, so a
    /// one-window pointwise dominance persists forever).
    pub fn min(&self, other: &Counting) -> Counting {
        // Compare long-run rates slope/period as cross products (u128: no overflow).
        let ra = self.slope as u128 * other.period as u128;
        let rb = other.slope as u128 * self.period as u128;
        if ra == rb {
            let p = lcm(self.period, other.period);
            let t = self.transient.max(other.transient);
            let slope = mul(self.slope, p / self.period);
            let samples = (0..t as u64 + p).map(|i| self.eval(i).min(other.eval(i))).collect();
            return Counting { samples, transient: t, period: p, slope }.normalize();
        }
        let (lo, hi) = if ra < rb { (self, other) } else { (other, self) };
        let p = lcm(lo.period, hi.period);
        let q_lo = mul(lo.slope, p / lo.period);
        let q_hi = mul(hi.slope, p / hi.period);
        debug_assert!(q_lo < q_hi);
        let t0 = lo.transient.max(hi.transient) as u64;
        // Largest pointwise deficit of lo below... above hi on the first window.
        let deficit = (t0..t0 + p)
            .map(|t| lo.eval(t).saturating_sub(hi.eval(t)))
            .max()
            .unwrap();
        // After k periods the deficit at each residue has shrunk by k*(q_hi-q_lo),
        // so from `cut` onward lo <= hi pointwise, forever.
        let k = deficit / (q_hi - q_lo) + 1;
        let cut = addu(t0, mul(k, p));
        let len = addu(cut, lo.period);
        let samples = (0..len).map(|t| lo.eval(t).min(hi.eval(t))).collect();
        Counting {
            samples,
            transient: cut as usize,
            period: lo.period,
            slope: lo.slope,
        }
        .normalize()
    }

    /// Ratio gearing: `N'(t) = mul * floor(N(t) / div)`.
    /// This is the counting semantics of a recipe leg that consumes `div`
    /// and produces `mul`.
    pub fn scale_floor(&self, mul_by: u64, div_by: u64) -> Counting {
        assert!(div_by >= 1, "division by zero ratio");
        // Over div/g periods the count grows by (div/g)*slope, a multiple of
        // div, so floor-division becomes exactly periodic there.
        let g = gcd(self.slope, div_by);
        let p = mul(self.period, div_by / g);
        let slope = mul(mul_by, self.slope / g);
        let t = self.transient;
        let samples = (0..t as u64 + p)
            .map(|i| mul(mul_by, self.eval(i) / div_by))
            .collect();
        Counting { samples, transient: t, period: p, slope }.normalize()
    }

    /// Pointwise difference `self - other`, defined only when the result is
    /// itself a monotone staircase (e.g. a pass-through split: total minus
    /// granted). Returns `None` otherwise. Exact: subtraction commutes with
    /// the shared period, no floors involved.
    pub(crate) fn monotone_sub(&self, other: &Counting) -> Option<Counting> {
        let p = lcm(self.period, other.period);
        let t = self.transient.max(other.transient);
        let slope = mul(self.slope, p / self.period)
            .checked_sub(mul(other.slope, p / other.period))?;
        let samples = (0..t as u64 + p)
            .map(|i| self.eval(i).checked_sub(other.eval(i)))
            .collect::<Option<Vec<u64>>>()?;
        Counting::try_from_parts(samples, t, p, slope)
    }

    /// Latency: `N'(t) = N(t - d)` (and 0 before `d`).
    pub fn shift(&self, d: u64) -> Counting {
        if d == 0 {
            return self.clone();
        }
        let t = self.transient as u64 + d;
        let samples = (0..t + self.period)
            .map(|i| if i < d { 0 } else { self.eval(i - d) })
            .collect();
        Counting {
            samples,
            transient: t as usize,
            period: self.period,
            slope: self.slope,
        }
        .normalize()
    }

    /// Canonicalize: minimal period, then minimal transient, then trim.
    fn normalize(mut self) -> Self {
        // 1. Minimal period: smallest divisor of `period` that generates the
        // tail. Checking the sample window suffices (chaining window steps
        // reproduces the original period relation).
        let p = self.period;
        for cand in (1..=p).filter(|c| p.is_multiple_of(*c)) {
            if !(self.slope as u128 * cand as u128).is_multiple_of(p as u128) {
                continue;
            }
            let scaled = self.slope * cand / p;
            let ok = (self.transient..self.transient + (p - cand) as usize)
                .all(|i| self.samples[i + cand as usize] == self.samples[i] + scaled);
            if ok {
                self.period = cand;
                self.slope = scaled;
                break;
            }
        }
        // 2. Minimal transient: extend the periodicity backwards while it holds.
        let p = self.period as usize;
        while self.transient > 0
            && self.samples[self.transient - 1 + p]
                == self.samples[self.transient - 1] + self.slope
        {
            self.transient -= 1;
        }
        // 3. Trim.
        self.samples.truncate(self.transient + p);
        self.samples.shrink_to_fit();
        debug_assert!(self.validate());
        self
    }

    /// Check structural invariants (used in tests / debug builds).
    pub fn validate(&self) -> bool {
        self.period >= 1
            && self.samples.len() == self.transient + self.period as usize
            && self.samples.windows(2).all(|w| w[0] <= w[1])
            && self.samples[self.transient] + self.slope
                >= *self.samples.last().unwrap()
    }

    fn is_well_formed(&self) -> bool {
        self.period >= 1
            && self.samples.len() == self.transient + self.period as usize
            && self.samples.windows(2).all(|w| w[0] <= w[1])
            && self.samples[self.transient] + self.slope >= *self.samples.last().unwrap()
    }

    fn check_well_formed(&self) {
        assert!(self.period >= 1, "period must be >= 1");
        assert_eq!(
            self.samples.len(),
            self.transient + self.period as usize,
            "samples must cover [0, transient + period)"
        );
        assert!(
            self.samples.windows(2).all(|w| w[0] <= w[1]),
            "samples must be non-decreasing"
        );
        assert!(
            self.samples[self.transient] + self.slope >= *self.samples.last().unwrap(),
            "seam must be monotone: N(transient+period) >= N(transient+period-1)"
        );
    }

    /// Accessors for engine layers above.
    pub fn transient_len(&self) -> usize {
        self.transient
    }
    pub fn period(&self) -> u64 {
        self.period
    }
    pub fn slope(&self) -> u64 {
        self.slope
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Deterministic xorshift64* PRNG — zero-dependency property tests.
    pub struct Rng(pub u64);

    impl Rng {
        pub fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545F4914F6CDD1D)
        }

        pub fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Dense reference evaluation of a Counting on [0, len).
    pub fn dense(c: &Counting, len: u64) -> Vec<u64> {
        (0..len).map(|t| c.eval(t)).collect()
    }

    /// Random canonical Counting with small parameters.
    pub fn random_counting(rng: &mut Rng) -> Counting {
        let transient = rng.below(6) as usize;
        let period = rng.below(4) + 1;
        let mut slope = rng.below(7);
        let mut samples = Vec::with_capacity(transient + period as usize);
        let mut v = rng.below(5);
        for _ in 0..transient + period as usize {
            samples.push(v);
            v += rng.below(3);
        }
        // Seam monotonicity: N(transient) + slope >= last sample.
        let deficit = samples.last().unwrap().saturating_sub(samples[transient]);
        if slope < deficit {
            slope = deficit;
        }
        Counting::from_parts(samples, transient, period, slope)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::collections::HashMap;

    const WINDOW: u64 = 400;
    const CASES: usize = 300;

    #[test]
    fn suffix_and_concat_are_exact() {
        let mut rng = Rng(0xfeed_beef_0451_cafe);
        for _ in 0..CASES {
            let c = random_counting(&mut rng);
            let t0 = rng.below(60);
            // suffix: pointwise re-based tail.
            let s = c.suffix(t0);
            assert!(s.validate(), "suffix invalid: {s:?}");
            for tau in 0..WINDOW {
                assert_eq!(s.eval(tau), c.eval(t0 + tau) - c.eval(t0),
                    "suffix t0={t0} tau={tau} c={c:?}");
            }
            // concat: prefix then re-based tail, pointwise.
            let g = random_counting(&mut rng);
            let d = c.concat(t0, &g);
            assert!(d.validate(), "concat invalid: {d:?}");
            for t in 0..WINDOW {
                let want = if t < t0 { c.eval(t) } else { c.eval(t0) + g.eval(t - t0) };
                assert_eq!(d.eval(t), want, "concat t0={t0} t={t} c={c:?} g={g:?}");
            }
            // The seam round-trip law: replaying your own tail is a no-op.
            assert_eq!(c.concat(t0, &c.suffix(t0)), c, "roundtrip t0={t0} c={c:?}");
        }
    }

    #[test]
    fn eval_matches_recurrence() {
        let mut rng = Rng(0x1234_5678_9abc_def0);
        for _ in 0..CASES {
            let c = random_counting(&mut rng);
            assert!(c.validate(), "invalid: {c:?}");
            // Independent recurrence: v[t] = samples-region, then v[t-p] + q.
            let mut v = vec![0u64; WINDOW as usize];
            for t in 0..WINDOW as usize {
                let window = c.transient_len() + c.period() as usize;
                v[t] = if t < window {
                    c.eval(t as u64) // inside the stored window eval is a direct lookup
                } else {
                    v[t - c.period() as usize] + c.slope()
                };
            }
            for t in 0..WINDOW {
                assert_eq!(c.eval(t), v[t as usize], "t={t} c={c:?}");
            }
            // Monotone everywhere.
            for t in 1..WINDOW {
                assert!(c.eval(t) >= c.eval(t - 1), "not monotone at {t}: {c:?}");
            }
        }
    }

    #[test]
    fn add_is_pointwise_sum() {
        let mut rng = Rng(0xfeed_face_cafe_beef);
        for _ in 0..CASES {
            let a = random_counting(&mut rng);
            let b = random_counting(&mut rng);
            let s = a.add(&b);
            assert!(s.validate());
            for t in 0..WINDOW {
                assert_eq!(s.eval(t), a.eval(t) + b.eval(t), "t={t}\na={a:?}\nb={b:?}");
            }
        }
    }

    #[test]
    fn min_is_pointwise_min() {
        let mut rng = Rng(0x00dd_ba11_0fc0_ffee);
        for _ in 0..CASES {
            let a = random_counting(&mut rng);
            let b = random_counting(&mut rng);
            let m = a.min(&b);
            assert!(m.validate());
            for t in 0..WINDOW {
                assert_eq!(
                    m.eval(t),
                    a.eval(t).min(b.eval(t)),
                    "t={t}\na={a:?}\nb={b:?}\nm={m:?}"
                );
            }
        }
    }

    #[test]
    fn scale_floor_matches_formula() {
        let mut rng = Rng(0x5eed_5eed_5eed_5eed);
        for _ in 0..CASES {
            let a = random_counting(&mut rng);
            let mul_by = rng.below(4) + 1;
            let div_by = rng.below(6) + 1;
            let s = a.scale_floor(mul_by, div_by);
            assert!(s.validate());
            for t in 0..WINDOW {
                assert_eq!(s.eval(t), mul_by * (a.eval(t) / div_by), "t={t} a={a:?}");
            }
        }
    }

    #[test]
    fn shift_matches_formula() {
        let mut rng = Rng(0xd15c_0b41_d15c_0b41);
        for _ in 0..CASES {
            let a = random_counting(&mut rng);
            let d = rng.below(10);
            let s = a.shift(d);
            assert!(s.validate());
            for t in 0..WINDOW {
                let expect = if t < d { 0 } else { a.eval(t - d) };
                assert_eq!(s.eval(t), expect, "t={t} d={d} a={a:?}");
            }
            assert_eq!(a.shift(0), a);
            assert_eq!(a.shift(3).shift(4), a.shift(7));
        }
    }

    #[test]
    fn normalization_is_canonical() {
        // Inflate a canonical value (multiply the period, pad the transient),
        // rebuild from dense samples, and require exact structural equality
        // after normalization. This is the cache-key property.
        let mut rng = Rng(0xca11_ab1e_ca11_ab1e);
        for _ in 0..CASES {
            let c = random_counting(&mut rng);
            let m = rng.below(3) + 1;
            let pad = rng.below(5);
            let period = c.period() * m;
            let transient = c.transient_len() as u64 + pad;
            let samples = dense(&c, transient + period);
            let inflated = Counting::from_parts(
                samples,
                transient as usize,
                period,
                c.slope() * m,
            );
            assert_eq!(inflated, c, "inflation m={m} pad={pad} not canonicalized");
        }
    }

    #[test]
    fn algebra_laws() {
        let mut rng = Rng(0x001a_550f_a19e_b4a0);
        for _ in 0..CASES {
            let a = random_counting(&mut rng);
            let b = random_counting(&mut rng);
            let c = random_counting(&mut rng);
            // Because normalization is canonical, semantic laws hold as
            // structural equality.
            assert_eq!(a.add(&b), b.add(&a));
            assert_eq!(a.add(&b).add(&c), a.add(&b.add(&c)));
            assert_eq!(a.min(&b), b.min(&a));
            assert_eq!(a.min(&b).min(&c), a.min(&b.min(&c)));
            assert_eq!(a.min(&a), a);
            assert_eq!(a.add(&Counting::zero()), a);
            assert_eq!(a.min(&a.add(&b)), a); // a <= a + b pointwise
            assert_eq!(a.scale_floor(1, 1), a);
        }
    }

    #[test]
    fn rates_compose() {
        let mut rng = Rng(0x4a7e_4a7e_4a7e_4a7e);
        for _ in 0..CASES {
            let a = random_counting(&mut rng);
            let b = random_counting(&mut rng);
            let (an, ad) = a.rate();
            let (bn, bd) = b.rate();
            // add: rate is the sum of rates.
            let (sn, sd) = a.add(&b).rate();
            assert_eq!((an * bd + bn * ad) * sd, sn * ad * bd);
            // min: rate is the min of rates.
            let (mn, md) = a.min(&b).rate();
            let min_is_a = an * bd <= bn * ad;
            let (en, ed) = if min_is_a { (an, ad) } else { (bn, bd) };
            assert_eq!(mn * ed, en * md);
        }
    }

    #[test]
    fn counting_is_a_cache_key() {
        // Same behavior built two different ways lands on the same map entry.
        let mut cache: HashMap<Counting, &'static str> = HashMap::new();
        let via_gears = Counting::unit_rate().scale_floor(2, 1).scale_floor(1, 2);
        cache.insert(via_gears, "hit");
        assert_eq!(cache.get(&Counting::unit_rate()), Some(&"hit"));
    }

    #[test]
    fn gear_two_thirds() {
        // A 2:3 gearbox on a full belt: rate 2 items per 3 ticks.
        let geared = Counting::unit_rate().scale_floor(2, 3);
        assert_eq!(geared.rate(), (2, 3));
        assert_eq!(geared.eval(0), 0);
        assert_eq!(geared.eval(3), 2);
        assert_eq!(geared.eval(7), 4);
    }
}
