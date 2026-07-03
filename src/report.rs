//! Player-visible module summaries and the conservation audit.
//!
//! The [`Summary`] is the "cache entry as spec" from DESIGN.md: exact
//! rational rates and first-arrival ticks per port, derived from the same
//! evaluation the engine caches.
//!
//! The [`Audit`] is the anti-item-duping ledger. Per item type it reports
//! exact long-run rates for injected / minted / consumed / delivered /
//! discarded flows and the accumulation rate (items piling up on wires),
//! and it *checks*, with exact counting-map algebra:
//!
//! 1. **No conjuring:** on every wire, cumulative consumption never exceeds
//!    cumulative supply (verified pointwise-forever via `min`).
//! 2. **The books close:** per type,
//!    `injected + minted == consumed + delivered + discarded + accumulation`
//!    as exact rationals.

use std::collections::{BTreeMap, HashSet};

use crate::counting::Counting;
use crate::eval::{EvalError, Evaluator, Trace};
use crate::net::{ItemType, Layout, NetId, Node, Source};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortReport {
    pub ty: ItemType,
    /// Exact long-run rate, items per tick, reduced.
    pub rate: (u64, u64),
    /// First tick with a nonzero count.
    pub first: Option<u64>,
}

/// The player-visible contract of a design under given input flows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub inputs: Vec<PortReport>,
    pub outputs: Vec<PortReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeBalance {
    pub ty: ItemType,
    /// Rate entering through net inputs (connected ones).
    pub injected: (u64, u64),
    /// Rate produced by recipe legs.
    pub minted: (u64, u64),
    /// Rate consumed by recipe legs.
    pub consumed: (u64, u64),
    /// Rate leaving through net outputs.
    pub delivered: (u64, u64),
    /// Rate produced on output legs wired to nothing.
    pub discarded: (u64, u64),
    /// Rate of items accumulating on wires (backlog growth).
    pub accumulating: (u64, u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audit {
    pub types: Vec<TypeBalance>,
}

// --- exact rational helpers (nonnegative, panic loudly on overflow) -------

fn rreduce(n: u128, d: u128) -> (u64, u64) {
    fn g(a: u128, b: u128) -> u128 {
        if b == 0 { a } else { g(b, a % b) }
    }
    if n == 0 {
        return (0, 1);
    }
    let k = g(n, d);
    ((n / k).try_into().expect("rate overflow"), (d / k).try_into().expect("rate overflow"))
}

fn radd(a: (u64, u64), b: (u64, u64)) -> (u64, u64) {
    rreduce(
        a.0 as u128 * b.1 as u128 + b.0 as u128 * a.1 as u128,
        a.1 as u128 * b.1 as u128,
    )
}

fn rscale(a: (u64, u64), k: u64) -> (u64, u64) {
    rreduce(a.0 as u128 * k as u128, a.1 as u128)
}

/// `a - b`, requiring `a >= b` (conservation guarantees it; violation is an
/// engine bug).
fn rsub(a: (u64, u64), b: (u64, u64)) -> (u64, u64) {
    let num = (a.0 as u128 * b.1 as u128)
        .checked_sub(b.0 as u128 * a.1 as u128)
        .expect("negative accumulation: conservation violated");
    rreduce(num, a.1 as u128 * b.1 as u128)
}

impl<'l> Evaluator<'l> {
    /// The published contract: exact port rates and first arrivals for a
    /// design under the given input flows. Memoized like any evaluation.
    pub fn summarize(
        &mut self,
        id: NetId,
        inputs: &[Counting],
    ) -> Result<Summary, EvalError> {
        let outs = self.evaluate(id, inputs)?;
        let net = self.lib().get(id);
        let report = |ty: ItemType, c: &Counting| PortReport {
            ty,
            rate: c.rate(),
            first: c.first_nonzero(),
        };
        Ok(Summary {
            inputs: net
                .inputs
                .iter()
                .zip(inputs)
                .map(|(&ty, c)| report(ty, c))
                .collect(),
            outputs: net
                .outputs
                .iter()
                .zip(&outs)
                .map(|(&ty, c)| report(ty, c))
                .collect(),
        })
    }

    /// Run the conservation audit on a design under the given input flows.
    pub fn audit(&mut self, id: NetId, inputs: &[Counting]) -> Result<Audit, EvalError> {
        let trace = self.trace_flattened(id, inputs)?;
        Ok(audit_trace(self, &trace))
    }
}

fn audit_trace(ev: &Evaluator<'_>, trace: &Trace) -> Audit {
    let net = &trace.net;
    let layout = Layout::new(ev.lib(), net);

    #[derive(Clone)]
    struct Acc {
        injected: (u64, u64),
        minted: (u64, u64),
        consumed: (u64, u64),
        delivered: (u64, u64),
        discarded: (u64, u64),
    }
    let mut acc: BTreeMap<ItemType, Acc> = BTreeMap::new();
    fn entry(m: &mut BTreeMap<ItemType, Acc>, ty: ItemType) -> &mut Acc {
        const ZERO: (u64, u64) = (0, 1);
        m.entry(ty).or_insert(Acc {
            injected: ZERO,
            minted: ZERO,
            consumed: ZERO,
            delivered: ZERO,
            discarded: ZERO,
        })
    }

    let mut used_sources: HashSet<Source> = HashSet::new();
    for w in &net.wires {
        for &s in &w.sources {
            used_sources.insert(s);
        }
    }

    // Injected: connected net inputs.
    for (i, &ty) in net.inputs.iter().enumerate() {
        if used_sources.contains(&Source::Input(i as u32)) {
            let e = entry(&mut acc, ty);
            e.injected = radd(e.injected, trace.inputs[i].rate());
        }
    }

    // Minted / discarded / consumed, per node.
    let mut wire_backlog_ok = true;
    for (n, node) in net.nodes.iter().enumerate() {
        let Node::Recipe { recipe, in_types, out_types } = node else {
            unreachable!("audit runs on flattened nets")
        };
        for (j, &ty) in out_types.iter().enumerate() {
            let rate = trace.node_outs[n][j].rate();
            let src = Source::NodeOut { node: n as u32, leg: j as u32 };
            let e = entry(&mut acc, ty);
            e.minted = radd(e.minted, rate);
            if !used_sources.contains(&src) {
                e.discarded = radd(e.discarded, rate);
            }
        }
        for (l, &ty) in in_types.iter().enumerate() {
            let consumed_rate = rscale(trace.firings[n].rate(), recipe.consume[l]);
            let e = entry(&mut acc, ty);
            e.consumed = radd(e.consumed, consumed_rate);
            // No conjuring: cumulative consumption <= cumulative supply on
            // this wire, checked pointwise-forever with exact algebra.
            let consumed_map = trace.firings[n].scale_floor(recipe.consume[l], 1);
            let supply = wire_supply(net, &layout, n, l, trace);
            if consumed_map.min(&supply) != consumed_map {
                wire_backlog_ok = false;
            }
        }
    }
    assert!(wire_backlog_ok, "conservation violated: a wire consumed more than it carried");

    // Delivered: net output wires.
    for (o, &ty) in net.outputs.iter().enumerate() {
        let e = entry(&mut acc, ty);
        e.delivered = radd(e.delivered, trace.outputs[o].rate());
    }

    let types = acc
        .into_iter()
        .map(|(ty, a)| {
            let inflow = radd(a.injected, a.minted);
            let outflow = radd(radd(a.consumed, a.delivered), a.discarded);
            let accumulating = rsub(inflow, outflow);
            TypeBalance {
                ty,
                injected: a.injected,
                minted: a.minted,
                consumed: a.consumed,
                delivered: a.delivered,
                discarded: a.discarded,
                accumulating,
            }
        })
        .collect();
    Audit { types }
}

/// Recompute the counting map on the wire feeding node `n`, leg `l`.
fn wire_supply(
    net: &crate::net::Net,
    layout: &Layout,
    n: usize,
    l: usize,
    trace: &Trace,
) -> Counting {
    let wire = &net.wires[layout.node_input_wire(n, l)];
    let mut acc = Counting::constant(wire.marking);
    for src in &wire.sources {
        let c = match src {
            Source::Input(i) => &trace.inputs[*i as usize],
            Source::NodeOut { node, leg } => &trace.node_outs[*node as usize][*leg as usize],
        };
        acc = acc.add(c);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::throttle;
    use crate::net::{Library, NetBuilder};
    use crate::recipe::Recipe;

    const IRON: ItemType = ItemType(0);
    const GEAR: ItemType = ItemType(1);
    const TOKEN: ItemType = ItemType(2);

    #[test]
    fn summary_is_the_published_contract() {
        let mut lib = Library::new();
        let mut b = NetBuilder::new();
        let iron = b.input(IRON);
        let n = b.recipe(Recipe::new(vec![2], vec![1], 3), &[IRON], &[GEAR]);
        let out = b.output(GEAR);
        b.connect(iron, n.input(0));
        b.connect(n.output(0), out);
        let id = lib.intern(b.build()).unwrap();

        let mut ev = Evaluator::new(&lib);
        let s = ev.summarize(id, &[Counting::unit_rate()]).unwrap();
        // "1/tick iron in -> 1/2 per tick gears out, first gear at t=5."
        assert_eq!(s.inputs[0].rate, (1, 1));
        assert_eq!(s.outputs[0].ty, GEAR);
        assert_eq!(s.outputs[0].rate, (1, 2));
        assert_eq!(s.outputs[0].first, Some(5));
    }

    #[test]
    fn audit_balances_a_transmuting_chain() {
        let mut lib = Library::new();
        let mut b = NetBuilder::new();
        let iron = b.input(IRON);
        let n = b.recipe(Recipe::new(vec![2], vec![1], 3), &[IRON], &[GEAR]);
        let out = b.output(GEAR);
        b.connect(iron, n.input(0));
        b.connect(n.output(0), out);
        let id = lib.intern(b.build()).unwrap();

        let mut ev = Evaluator::new(&lib);
        let audit = ev.audit(id, &[Counting::unit_rate()]).unwrap();
        let iron_row = audit.types.iter().find(|t| t.ty == IRON).unwrap();
        assert_eq!(iron_row.injected, (1, 1));
        assert_eq!(iron_row.consumed, (1, 1));
        assert_eq!(iron_row.accumulating, (0, 1));
        let gear_row = audit.types.iter().find(|t| t.ty == GEAR).unwrap();
        assert_eq!(gear_row.minted, (1, 2));
        assert_eq!(gear_row.delivered, (1, 2));
        assert_eq!(gear_row.accumulating, (0, 1));
    }

    #[test]
    fn audit_sees_belt_backlog_on_a_throttled_line() {
        // Feed 1/tick into a machine that only passes 3/4: the audit must
        // report the input belt jamming at exactly 1/4 per tick.
        let mut lib = Library::new();
        let id = throttle(&mut lib, IRON, TOKEN, 3, 4);
        let mut ev = Evaluator::new(&lib);
        let audit = ev.audit(id, &[Counting::unit_rate()]).unwrap();
        let iron_row = audit.types.iter().find(|t| t.ty == IRON).unwrap();
        assert_eq!(iron_row.injected, (1, 1));
        assert_eq!(iron_row.delivered, (3, 4));
        assert_eq!(iron_row.accumulating, (1, 4));
        // Tokens circulate losslessly.
        let tok_row = audit.types.iter().find(|t| t.ty == TOKEN).unwrap();
        assert_eq!(tok_row.minted, tok_row.consumed);
        assert_eq!(tok_row.accumulating, (0, 1));
    }

    #[test]
    fn audit_reports_discarded_output_legs() {
        // Round-robin splitter with the right leg wired to nothing.
        let mut lib = Library::new();
        let mut b = NetBuilder::new();
        let iron = b.input(IRON);
        let n = b.recipe(Recipe::new(vec![2], vec![1, 1], 0), &[IRON], &[IRON, IRON]);
        let out = b.output(IRON);
        b.connect(iron, n.input(0));
        b.connect(n.output(0), out);
        // n.output(1) deliberately dangling.
        let id = lib.intern(b.build()).unwrap();

        let mut ev = Evaluator::new(&lib);
        let audit = ev.audit(id, &[Counting::unit_rate()]).unwrap();
        let row = audit.types.iter().find(|t| t.ty == IRON).unwrap();
        assert_eq!(row.discarded, (1, 2));
        assert_eq!(row.delivered, (1, 2));
        assert_eq!(row.accumulating, (0, 1));
    }
}
