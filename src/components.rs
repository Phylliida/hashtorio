//! Derived components: the DESIGN.md standard library, built from nothing
//! but recipes, markings, and wiring. Each constructor is a one-liner-ish
//! net; the tests are the proof that the derivations behave as claimed.

use crate::net::{ItemType, Library, NetBuilder, NetId};
use crate::recipe::Recipe;

/// Clock: a self-loop recipe `token -> token + pulse` with latency `period`
/// and one preloaded token. Emits one pulse every `period` ticks, first at
/// `t = period`.
pub fn clock(lib: &mut Library, period: u64, token: ItemType, pulse: ItemType) -> NetId {
    assert!(period >= 1, "a zero-period clock is a zero-latency cycle");
    let mut b = NetBuilder::new();
    let n = b.recipe(
        Recipe::new(vec![1], vec![1, 1], period),
        &[token],
        &[token, pulse],
    );
    let out = b.output(pulse);
    b.connect(n.output(0), n.input(0));
    b.marking(n.input(0), 1);
    b.connect(n.output(1), out);
    lib.intern(b.build()).expect("clock net is valid")
}

/// Marked-loop throughput limiter: items pass through a machine that also
/// needs a token from a loop preloaded with `tokens` and taking `latency`
/// ticks per round trip. Long-run throughput = min(input rate,
/// tokens/latency) — the (min,+) critical-circuit law.
pub fn throttle(
    lib: &mut Library,
    item: ItemType,
    token: ItemType,
    tokens: u64,
    latency: u64,
) -> NetId {
    assert!(latency >= 1, "a zero-latency token loop is a zero-latency cycle");
    let mut b = NetBuilder::new();
    let input = b.input(item);
    let n = b.recipe(
        Recipe::new(vec![1, 1], vec![1, 1], latency),
        &[item, token],
        &[item, token],
    );
    let out = b.output(item);
    b.connect(input, n.input(0));
    b.connect(n.output(0), out);
    b.connect(n.output(1), n.input(1));
    b.marking(n.input(1), tokens);
    lib.intern(b.build()).expect("throttle net is valid")
}

/// Reservoir gauge: a consume-and-refund loop `K·item -> K·item + pulse`
/// (latency 1) over a reservoir preloaded with `reserve` items; `input`
/// tops the reservoir up. Pulse rate per tick = floor(level / K) — a level
/// meter built from pure recipes.
///
/// Note the honest limitation (see DESIGN.md): the gauge *sequesters* its
/// reservoir — the kernel's single-sink wires mean you cannot also drain
/// the same wire downstream. Metering a through-flow is the `tap` recipe
/// `item -> item + pulse` instead; level-sensing a drainable buffer is a
/// tier-1 (priority) pattern.
pub fn gauge(
    lib: &mut Library,
    item: ItemType,
    pulse: ItemType,
    threshold: u64,
    reserve: u64,
) -> NetId {
    assert!(threshold >= 1);
    let mut b = NetBuilder::new();
    let input = b.input(item);
    let n = b.recipe(
        Recipe::new(vec![threshold], vec![threshold, 1], 1),
        &[item],
        &[item, pulse],
    );
    let out = b.output(pulse);
    b.connect(input, n.input(0));
    b.connect(n.output(0), n.input(0));
    b.marking(n.input(0), reserve);
    b.connect(n.output(1), out);
    lib.intern(b.build()).expect("gauge net is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counting::Counting;
    use crate::eval::{EvalError, Evaluator};
    use crate::net::NetBuilder;

    const ITEM: ItemType = ItemType(0);
    const TOKEN: ItemType = ItemType(1);
    const PULSE: ItemType = ItemType(2);

    #[test]
    fn clock_ticks_at_its_period() {
        let mut lib = Library::new();
        let id = clock(&mut lib, 4, TOKEN, PULSE);
        let mut ev = Evaluator::new(&lib);
        let outs = ev.evaluate(id, &[]).unwrap();
        let pulses = &outs[0];
        assert_eq!(pulses.eval(3), 0);
        assert_eq!(pulses.eval(4), 1);
        assert_eq!(pulses.eval(11), 2);
        assert_eq!(pulses.rate(), (1, 4));
    }

    #[test]
    fn throttle_obeys_the_critical_circuit_law() {
        // 3 tokens on a 4-tick loop: throughput = 3/4 of a full belt.
        let mut lib = Library::new();
        let id = throttle(&mut lib, ITEM, TOKEN, 3, 4);
        let mut ev = Evaluator::new(&lib);
        let outs = ev.evaluate(id, &[Counting::unit_rate()]).unwrap();
        assert_eq!(outs[0].rate(), (3, 4));
        // Under-supplied (input rate 1/2 < 3/4): input-limited instead.
        let slow = Counting::unit_rate().scale_floor(1, 2);
        let outs = ev.evaluate(id, &[slow]).unwrap();
        assert_eq!(outs[0].rate(), (1, 2));
    }

    #[test]
    fn gauge_reads_its_reservoir_level() {
        let mut lib = Library::new();
        // Level 5 >= threshold 4: pulses every tick.
        let full = gauge(&mut lib, ITEM, PULSE, 4, 5);
        // Level 3 < threshold 4: silent.
        let low = gauge(&mut lib, ITEM, PULSE, 4, 3);
        let mut ev = Evaluator::new(&lib);
        let outs = ev.evaluate(full, &[Counting::zero()]).unwrap();
        assert_eq!(outs[0].rate(), (1, 1));
        let outs = ev.evaluate(low, &[Counting::zero()]).unwrap();
        assert_eq!(outs[0].rate(), (0, 1));
        // Topping the low reservoir up past threshold wakes the gauge.
        let outs = ev
            .evaluate(low, &[Counting::constant(2)])
            .unwrap();
        assert_eq!(outs[0].rate(), (1, 1));
    }

    #[test]
    fn feedback_through_a_module_boundary() {
        // The throttle core as a module with its token ports exposed; the
        // token loop is closed OUTSIDE, at the parent level. This forces the
        // flatten path (a cycle through a module is not a black box).
        let mut lib = Library::new();
        let core = {
            let mut b = NetBuilder::new();
            let item_in = b.input(ITEM);
            let tok_in = b.input(TOKEN);
            let n = b.recipe(
                Recipe::new(vec![1, 1], vec![1, 1], 4),
                &[ITEM, TOKEN],
                &[ITEM, TOKEN],
            );
            let item_out = b.output(ITEM);
            let tok_out = b.output(TOKEN);
            b.connect(item_in, n.input(0));
            b.connect(tok_in, n.input(1));
            b.connect(n.output(0), item_out);
            b.connect(n.output(1), tok_out);
            lib.intern(b.build()).unwrap()
        };
        let parent = {
            let mut b = NetBuilder::new();
            let item_in = b.input(ITEM);
            let m = b.module(&lib, core);
            let out = b.output(ITEM);
            b.connect(item_in, m.input(0));
            b.connect(m.output(0), out);
            b.connect(m.output(1), m.input(1)); // token loop through the module
            b.marking(m.input(1), 3);
            lib.intern(b.build()).unwrap()
        };
        let mut ev = Evaluator::new(&lib);
        let outs = ev.evaluate(parent, &[Counting::unit_rate()]).unwrap();
        assert_eq!(outs[0].rate(), (3, 4));

        // Same behavior as the directly-wired throttle.
        let flat_id = throttle(&mut lib, ITEM, TOKEN, 3, 4);
        let mut ev2 = Evaluator::new(&lib);
        let direct = ev2.evaluate(flat_id, &[Counting::unit_rate()]).unwrap();
        assert_eq!(outs, direct);
    }

    #[test]
    fn zero_latency_cycle_is_rejected() {
        let mut lib = Library::new();
        let mut b = NetBuilder::new();
        let n = b.recipe(Recipe::new(vec![1], vec![1], 0), &[ITEM], &[ITEM]);
        b.connect(n.output(0), n.input(0));
        b.marking(n.input(0), 1);
        let id = lib.intern(b.build()).unwrap();
        let mut ev = Evaluator::new(&lib);
        assert_eq!(ev.evaluate(id, &[]), Err(EvalError::ZeroLatencyCycle));
    }

    #[test]
    fn breeder_loop_has_no_linear_steady_state() {
        // 3·U -> 4·U on a self-loop: exponential growth. The summarizer must
        // refuse honestly rather than emit a bogus rate.
        let mut lib = Library::new();
        let mut b = NetBuilder::new();
        let n = b.recipe(Recipe::new(vec![3], vec![4], 2), &[ITEM], &[ITEM]);
        b.connect(n.output(0), n.input(0));
        b.marking(n.input(0), 3);
        let id = lib.intern(b.build()).unwrap();
        let mut ev = Evaluator::new(&lib);
        assert_eq!(ev.evaluate(id, &[]), Err(EvalError::RateExplosion));
    }

    #[test]
    fn starved_loop_reaches_a_dead_steady_state() {
        // A machine needing 2 tokens per firing from a loop that only ever
        // holds 1: fires never, output rate 0 — a deadlock, exactly summarized.
        let mut lib = Library::new();
        let mut b = NetBuilder::new();
        let input = b.input(ITEM);
        let n = b.recipe(
            Recipe::new(vec![1, 2], vec![1, 2], 3),
            &[ITEM, TOKEN],
            &[ITEM, TOKEN],
        );
        let out = b.output(ITEM);
        b.connect(input, n.input(0));
        b.connect(n.output(0), out);
        b.connect(n.output(1), n.input(1));
        b.marking(n.input(1), 1);
        let id = lib.intern(b.build()).unwrap();
        let mut ev = Evaluator::new(&lib);
        let outs = ev.evaluate(id, &[Counting::unit_rate()]).unwrap();
        assert_eq!(outs[0], Counting::zero());
    }
}
