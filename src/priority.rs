//! Exact evaluation of the Priority node on ultimately periodic inputs.
//!
//! The per-tick recurrence, with `I` = cumulative arriving items and `D` =
//! cumulative supplied tokens:
//!
//! ```text
//! used(t) = used(t-1) + min( I(t) - I(t-1),  D(t) - used(t-1) )
//! granted  = used          fallback = I - used
//! ```
//!
//! [`priority_apply`] derives `used` densely over a window and then *proves*
//! ultimate periodicity with one of two closure certificates:
//!
//! 1. **State repeat.** The reserve `r(t) = D(t) - used(t)` at two ticks with
//!    the same input phase (`t2 = t1 + m*P_in`, both past the input
//!    transients) is equal. The recurrence is deterministic and its future
//!    depends only on (reserve, input phase), so the trajectory from `t2`
//!    replays the one from `t1`: periodic with period `t2 - t1` from `t1`.
//!    The reserve is bounded whenever token supply doesn't outpace demand,
//!    and bounded reserve + periodic inputs pigeonholes into a repeat.
//! 2. **Token surplus.** Over one full final input period, every arriving
//!    batch was fully granted with reserve to spare
//!    (`D(t) - used(t-1) >= I(t) - I(t-1)`), and the token rate is at least
//!    the item rate. Then per-phase reserve never decreases across periods,
//!    so grants stay full forever: `used` tracks `I` exactly from there on.
//!
//! If neither certificate closes within the window, the caller retries with
//! a larger one. Soundness never depends on how the window was chosen.

use crate::counting::{gcd, Counting};

fn lcm_capped(a: u64, b: u64, cap: u64) -> u64 {
    let g = gcd(a, b);
    let l = (a / g) as u128 * b as u128;
    if l > cap as u128 {
        cap.saturating_add(1)
    } else {
        l as u64
    }
}

/// Derive `(granted, fallback)` exactly, or `None` if no certificate closes
/// within `window` ticks.
pub(crate) fn priority_apply(
    items: &Counting,
    tokens: &Counting,
    window: u64,
) -> Option<(Counting, Counting)> {
    let tj = items.transient_len().max(tokens.transient_len());
    let pj = lcm_capped(items.period(), tokens.period(), window);
    let w = window.max(16) as usize;
    if tj + 2 * pj as usize + 2 > w {
        return None; // window can't even hold two periods past the transient
    }

    // Dense derivation of the recurrence.
    let mut used = Vec::with_capacity(w);
    let mut i_prev = 0u64;
    let mut u_prev = 0u64;
    for t in 0..w as u64 {
        let i = items.eval(t);
        let d = tokens.eval(t);
        let delta_i = i - i_prev;
        let reserve = d - u_prev; // used(t-1) <= D(t-1) <= D(t): no underflow
        let du = delta_i.min(reserve);
        u_prev += du;
        used.push(u_prev);
        i_prev = i;
    }
    let reserve_at = |t: usize| tokens.eval(t as u64) - used[t];

    let build = |t0: usize, p: u64| -> Option<(Counting, Counting)> {
        let pu = p as usize;
        let granted = Counting::try_from_parts(
            used[..t0 + pu].to_vec(),
            t0,
            p,
            used[t0 + pu] - used[t0],
        )?;
        let fallback = items.monotone_sub(&granted)?;
        Some((granted, fallback))
    };

    // Certificate 1: reserve repeat at equal input phase.
    let t0_floor = tj.max(1);
    let mut m = 1u64;
    while t0_floor as u64 + m * pj < w as u64 {
        let lag = (m * pj) as usize;
        for t1 in t0_floor..w - lag - 1 {
            if reserve_at(t1) == reserve_at(t1 + lag) {
                return build(t1, m * pj);
            }
        }
        m += 1;
    }

    // Certificate 2: token surplus — full grants with headroom over the
    // last full period, and tokens at least as fast as items.
    let (ni, di_) = items.rate();
    let (nd, dd) = tokens.rate();
    if nd as u128 * di_ as u128 >= ni as u128 * dd as u128 {
        let pu = pj as usize;
        let t0 = w - 2 * pu;
        if t0 > tj {
            let full_grants = (t0..w).all(|t| {
                let delta_i = items.eval(t as u64) - if t == 0 { 0 } else { items.eval(t as u64 - 1) };
                let reserve_before = tokens.eval(t as u64) - if t == 0 { 0 } else { used[t - 1] };
                reserve_before >= delta_i
            });
            if full_grants {
                return build(t0, pj);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counting::test_support::{random_counting, Rng};

    /// Direct recurrence, no cleverness: the reference semantics.
    fn naive(items: &Counting, tokens: &Counting, len: u64) -> (Vec<u64>, Vec<u64>) {
        let mut used = Vec::new();
        let mut fall = Vec::new();
        let (mut i_prev, mut u_prev) = (0u64, 0u64);
        for t in 0..len {
            let i = items.eval(t);
            let du = (i - i_prev).min(tokens.eval(t) - u_prev);
            u_prev += du;
            used.push(u_prev);
            fall.push(i - u_prev);
            i_prev = i;
        }
        (used, fall)
    }

    #[test]
    fn matches_naive_recurrence() {
        let mut rng = Rng(0x9107_17e5_e1ec_7000);
        let mut closed = 0;
        for _ in 0..300 {
            let items = random_counting(&mut rng);
            let tokens = random_counting(&mut rng);
            let Some((granted, fallback)) = priority_apply(&items, &tokens, 512) else {
                continue; // slow-mixing case: legitimate, caller would widen
            };
            closed += 1;
            let (used, fall) = naive(&items, &tokens, 400);
            for t in 0..400u64 {
                assert_eq!(granted.eval(t), used[t as usize], "t={t}\nI={items:?}\nD={tokens:?}");
                assert_eq!(fallback.eval(t), fall[t as usize], "t={t}\nI={items:?}\nD={tokens:?}");
            }
            // Conservation through the gate, forever (symbolically).
            assert_eq!(granted.add(&fallback), items);
        }
        assert!(closed > 250, "closure rate suspiciously low: {closed}/300");
    }

    #[test]
    fn starved_and_flooded_extremes() {
        // No tokens at all: everything falls through.
        let items = Counting::unit_rate();
        let (granted, fallback) =
            priority_apply(&items, &Counting::zero(), 512).unwrap();
        assert_eq!(granted, Counting::zero());
        assert_eq!(fallback, items);
        // Token flood: everything is granted.
        let flood = Counting::unit_rate().scale_floor(10, 1);
        let (granted, fallback) = priority_apply(&items, &flood, 512).unwrap();
        assert_eq!(granted, items);
        assert_eq!(fallback, Counting::zero());
    }
}
