//! Tier 2: exact tick-stepping for nets with no linear steady state.
//!
//! When the summarizer honestly refuses (`RateExplosion`,
//! `NoPeriodicSteadyState`), the net still has exact semantics — it just
//! doesn't compress to an ultimately periodic map. The [`Stepper`] advances
//! a flattened net tick by tick in **delta-state** form:
//!
//! - per node-input wire: the *slack* (supplied minus consumed so far),
//! - per recipe node: a short window of recent firing deltas (as deep as
//!   any consumer's latency looks back),
//! - per priority node: the token reserve.
//!
//! Everything is relative — no absolute counts — so two instances of the
//! same design in the same [`StepState`] advance identically on identical
//! input deltas. That is what makes the [`ChunkCache`] sound: chunks of
//! `(design, state, input deltas) -> (state, output deltas)` are shared
//! across instances and across time, HashLife-style, even for behavior
//! that never becomes periodic.

use std::collections::HashMap;

use crate::eval::EvalError;
use crate::net::{Layout, Library, Net, Node, Source};

/// How a node-input wire is fed, precompiled for stepping.
enum StepSrc {
    Input(usize),
    /// `amt * Δk_src(t - lat)`.
    Recipe { src: usize, amt: u64, lat: u64 },
    /// Priority output, same tick: 0 = granted, 1 = fallback.
    Prio { src: usize, leg: u8 },
}

struct StepLeg {
    sources: Vec<StepSrc>,
    consume: u64,
}

enum StepKind {
    // Produce amounts live on consumers' StepSrc::Recipe entries.
    Recipe,
    Priority,
}

struct StepNode {
    kind: StepKind,
    legs: Vec<StepLeg>,
    /// How far back consumers look at this node's firing deltas.
    hist_len: usize,
}

/// The complete relative state of a stepped net. `Hash`/`Eq` make it a
/// sound chunk-cache key: equal states + equal input deltas => equal
/// futures.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepState {
    /// Per node-input wire: supplied minus consumed, cumulative.
    slack: Vec<u64>,
    /// Per node: firing deltas at ticks t-1, t-2, ... (length `hist_len`).
    hist: Vec<Vec<u64>>,
    /// Per priority node index: unconsumed token reserve.
    reserve: Vec<u64>,
}

pub struct Stepper {
    nodes: Vec<StepNode>,
    /// Node processing order within a tick (same-tick dependencies first).
    intra_order: Vec<usize>,
    /// Sources of each net output wire.
    out_sources: Vec<Vec<StepSrc>>,
    /// Output-wire markings, delivered at tick 0.
    out_pending: Vec<u64>,
    n_inputs: usize,
    state: StepState,
    /// Next tick to compute.
    t: u64,
}

impl Stepper {
    /// Prepare a flattened design for stepping.
    pub fn new(lib: &Library, id: crate::net::NetId) -> Result<Stepper, EvalError> {
        let flat = crate::flatten::flatten(lib, id).map_err(EvalError::Net)?;
        Self::from_flat(lib, &flat)
    }

    pub fn from_flat(lib: &Library, net: &Net) -> Result<Stepper, EvalError> {
        let layout = Layout::new(lib, net);
        let nn = net.nodes.len();

        let compile_source = |src: &Source| -> StepSrc {
            match src {
                Source::Input(i) => StepSrc::Input(*i as usize),
                Source::NodeOut { node, leg } => match &net.nodes[*node as usize] {
                    Node::Recipe { recipe, .. } => StepSrc::Recipe {
                        src: *node as usize,
                        amt: recipe.produce[*leg as usize],
                        lat: recipe.latency,
                    },
                    Node::Priority { .. } => {
                        StepSrc::Prio { src: *node as usize, leg: *leg as u8 }
                    }
                    Node::Module(_) => unreachable!("stepper takes flattened nets"),
                },
            }
        };

        let mut prio_index = vec![usize::MAX; nn];
        let mut n_prio = 0usize;
        let mut nodes: Vec<StepNode> = Vec::with_capacity(nn);
        let mut slack_init: Vec<u64> = Vec::new();
        for (g, node) in net.nodes.iter().enumerate() {
            let legs: Vec<StepLeg> = match node {
                Node::Recipe { recipe, .. } => recipe
                    .consume
                    .iter()
                    .enumerate()
                    .map(|(l, &c)| StepLeg {
                        sources: net.wires[layout.node_input_wire(g, l)]
                            .sources
                            .iter()
                            .map(compile_source)
                            .collect(),
                        consume: c,
                    })
                    .collect(),
                Node::Priority { .. } => {
                    prio_index[g] = n_prio;
                    n_prio += 1;
                    (0..2)
                        .map(|l| StepLeg {
                            sources: net.wires[layout.node_input_wire(g, l)]
                                .sources
                                .iter()
                                .map(compile_source)
                                .collect(),
                            consume: 1,
                        })
                        .collect()
                }
                Node::Module(_) => unreachable!("stepper takes flattened nets"),
            };
            for (l, _) in legs.iter().enumerate() {
                slack_init.push(net.wires[layout.node_input_wire(g, l)].marking);
            }
            let kind = match node {
                Node::Recipe { .. } => StepKind::Recipe,
                Node::Priority { .. } => StepKind::Priority,
                Node::Module(_) => unreachable!(),
            };
            nodes.push(StepNode { kind, legs, hist_len: 0 });
        }

        // History depth: the deepest latency anything (node legs or net
        // outputs) looks back at. Lookups happen before rotation, at index
        // lat-1, so a depth of `lat` suffices.
        {
            let mut depth = vec![0usize; nn];
            let mut note = |sources: &[StepSrc]| {
                for s in sources {
                    if let StepSrc::Recipe { src, lat, .. } = s {
                        depth[*src] = depth[*src].max(*lat as usize);
                    }
                }
            };
            for n in &nodes {
                for leg in &n.legs {
                    note(&leg.sources);
                }
            }
            for o in 0..net.outputs.len() {
                let compiled: Vec<StepSrc> = net.wires[layout.output_wire(o)]
                    .sources
                    .iter()
                    .map(compile_source)
                    .collect();
                note(&compiled);
            }
            for (n, d) in nodes.iter_mut().zip(depth) {
                n.hist_len = d;
            }
        }

        // Same-tick topological order (zero-latency recipe edges + all
        // priority edges); a cycle is Zeno.
        let intra_order = {
            let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nn];
            let mut indeg = vec![0usize; nn];
            for (g, n) in nodes.iter().enumerate() {
                for leg in &n.legs {
                    for s in &leg.sources {
                        let same_tick = match s {
                            StepSrc::Recipe { src, lat: 0, .. } => Some(*src),
                            StepSrc::Prio { src, .. } => Some(*src),
                            _ => None,
                        };
                        if let Some(src) = same_tick {
                            edges[src].push(g);
                            indeg[g] += 1;
                        }
                    }
                }
            }
            let mut queue: Vec<usize> = (0..nn).filter(|&i| indeg[i] == 0).collect();
            let mut order = Vec::with_capacity(nn);
            while let Some(v) = queue.pop() {
                order.push(v);
                for &w in &edges[v] {
                    indeg[w] -= 1;
                    if indeg[w] == 0 {
                        queue.push(w);
                    }
                }
            }
            if order.len() != nn {
                return Err(EvalError::ZeroLatencyCycle);
            }
            order
        };

        let out_sources: Vec<Vec<StepSrc>> = (0..net.outputs.len())
            .map(|o| {
                net.wires[layout.output_wire(o)]
                    .sources
                    .iter()
                    .map(compile_source)
                    .collect()
            })
            .collect();
        // Output-wire markings are delivered at tick 0.
        let out_pending: Vec<u64> = (0..net.outputs.len())
            .map(|o| net.wires[layout.output_wire(o)].marking)
            .collect();

        let hist = nodes.iter().map(|n| vec![0u64; n.hist_len]).collect();
        Ok(Stepper {
            nodes,
            intra_order,
            out_sources,
            out_pending,
            n_inputs: net.inputs.len(),
            state: StepState { slack: slack_init, hist, reserve: vec![0; n_prio] },
            t: 0,
        })
    }

    pub fn tick(&self) -> u64 {
        self.t
    }

    pub fn state(&self) -> &StepState {
        &self.state
    }

    pub fn set_state(&mut self, state: StepState, t: u64) {
        self.state = state;
        self.t = t;
    }

    /// Advance one tick. `input_deltas[i]` = items arriving on net input `i`
    /// this tick. Returns output deltas per net output.
    pub fn step(&mut self, input_deltas: &[u64]) -> Result<Vec<u64>, EvalError> {
        assert_eq!(input_deltas.len(), self.n_inputs, "input arity");
        let nn = self.nodes.len();
        // This tick's deltas: recipes k; priorities (granted, fallback).
        let mut cur_k = vec![0u64; nn];
        let mut cur_prio = vec![(0u64, 0u64); nn];

        let mut leg_base = vec![0usize; nn];
        let mut prio_of = vec![usize::MAX; nn];
        {
            let (mut c, mut pi) = (0usize, 0usize);
            for (g, n) in self.nodes.iter().enumerate() {
                leg_base[g] = c;
                c += n.legs.len();
                if matches!(n.kind, StepKind::Priority) {
                    prio_of[g] = pi;
                    pi += 1;
                }
            }
        }

        // Δ of a source at the current tick; pre-rotation, so a recipe's
        // firing delta at t-lat sits at hist index lat-1.
        macro_rules! src_delta {
            ($s:expr) => {
                match *$s {
                    StepSrc::Input(i) => input_deltas[i],
                    StepSrc::Recipe { src, amt, lat } => {
                        let dk = if lat == 0 {
                            cur_k[src]
                        } else if self.t >= lat {
                            self.state.hist[src][lat as usize - 1]
                        } else {
                            0
                        };
                        amt.checked_mul(dk).ok_or(EvalError::RateExplosion)?
                    }
                    StepSrc::Prio { src, leg } => {
                        if leg == 0 {
                            cur_prio[src].0
                        } else {
                            cur_prio[src].1
                        }
                    }
                }
            };
        }

        let order = self.intra_order.clone();
        for &g in &order {
            let base = leg_base[g];
            let n_legs = self.nodes[g].legs.len();
            match self.nodes[g].kind {
                StepKind::Recipe => {
                    let mut fire = u64::MAX;
                    for l in 0..n_legs {
                        let mut dn = 0u64;
                        for si in 0..self.nodes[g].legs[l].sources.len() {
                            let d = src_delta!(&self.nodes[g].legs[l].sources[si]);
                            dn = dn.checked_add(d).ok_or(EvalError::RateExplosion)?;
                        }
                        let avail = self.state.slack[base + l]
                            .checked_add(dn)
                            .ok_or(EvalError::RateExplosion)?;
                        self.state.slack[base + l] = avail;
                        fire = fire.min(avail / self.nodes[g].legs[l].consume);
                    }
                    for l in 0..n_legs {
                        self.state.slack[base + l] -= self.nodes[g].legs[l].consume * fire;
                    }
                    cur_k[g] = fire;
                }
                StepKind::Priority => {
                    let mut leg_delta = [0u64; 2];
                    for (l, ld) in leg_delta.iter_mut().enumerate() {
                        let mut dn = 0u64;
                        for si in 0..self.nodes[g].legs[l].sources.len() {
                            let d = src_delta!(&self.nodes[g].legs[l].sources[si]);
                            dn = dn.checked_add(d).ok_or(EvalError::RateExplosion)?;
                        }
                        *ld = dn;
                    }
                    // Initial markings sit in the slacks until first use.
                    let di = leg_delta[0] + self.state.slack[base];
                    self.state.slack[base] = 0;
                    let rho = self.state.reserve[prio_of[g]]
                        .checked_add(leg_delta[1] + self.state.slack[base + 1])
                        .ok_or(EvalError::RateExplosion)?;
                    self.state.slack[base + 1] = 0;
                    let du = di.min(rho);
                    self.state.reserve[prio_of[g]] = rho - du;
                    cur_prio[g] = (du, di - du);
                }
            }
        }

        // Output deltas (pre-rotation, same lookup convention).
        let mut outs = Vec::with_capacity(self.out_sources.len());
        for (o, sources) in self.out_sources.iter().enumerate() {
            let mut d = if self.t == 0 { self.out_pending[o] } else { 0 };
            for s in sources {
                d += match *s {
                    StepSrc::Input(i) => input_deltas[i],
                    StepSrc::Recipe { src, amt, lat } => {
                        let dk = if lat == 0 {
                            cur_k[src]
                        } else if self.t >= lat {
                            self.state.hist[src][lat as usize - 1]
                        } else {
                            0
                        };
                        amt * dk
                    }
                    StepSrc::Prio { src, leg } => {
                        if leg == 0 {
                            cur_prio[src].0
                        } else {
                            cur_prio[src].1
                        }
                    }
                };
            }
            outs.push(d);
        }

        // Push history windows (delta of tick t moves to the front).
        for (g, n) in self.nodes.iter().enumerate() {
            if n.hist_len > 0 {
                let h = &mut self.state.hist[g];
                h.rotate_right(1);
                h[0] = cur_k[g];
            }
        }
        self.t += 1;
        Ok(outs)
    }
}

// ---------------------------------------------------------------------------
// Chunked memoization
// ---------------------------------------------------------------------------

/// Number of ticks advanced per cached chunk.
pub const CHUNK: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChunkKey {
    design: u32,
    state: StepState,
    inputs: Vec<Vec<u64>>,
}

#[derive(Debug, Clone)]
struct ChunkVal {
    state: StepState,
    outputs: Vec<Vec<u64>>,
}

/// Shared cache of `(design, state, input chunk) -> (state, output chunk)`.
#[derive(Default)]
pub struct ChunkCache {
    map: HashMap<ChunkKey, ChunkVal>,
    pub hits: u64,
    pub misses: u64,
}

impl ChunkCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance a stepper by one chunk, through the cache. `input_deltas`
    /// must contain exactly [`CHUNK`] ticks of per-input deltas.
    pub fn advance(
        &mut self,
        design: u32,
        stepper: &mut Stepper,
        input_deltas: &[Vec<u64>],
    ) -> Result<Vec<Vec<u64>>, EvalError> {
        assert_eq!(input_deltas.len(), CHUNK);
        let key = ChunkKey {
            design,
            state: stepper.state().clone(),
            inputs: input_deltas.to_vec(),
        };
        if let Some(hit) = self.map.get(&key) {
            self.hits += 1;
            let t = stepper.tick() + CHUNK as u64;
            stepper.set_state(hit.state.clone(), t);
            return Ok(hit.outputs.clone());
        }
        self.misses += 1;
        let mut outputs = Vec::with_capacity(CHUNK);
        for step_in in input_deltas {
            outputs.push(stepper.step(step_in)?);
        }
        self.map.insert(
            key,
            ChunkVal { state: stepper.state().clone(), outputs: outputs.clone() },
        );
        Ok(outputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{demand_store, throttle};
    use crate::counting::Counting;
    use crate::eval::Evaluator;
    use crate::net::{ItemType, NetBuilder};
    use crate::recipe::Recipe;

    const ITEM: ItemType = ItemType(0);
    const TOKEN: ItemType = ItemType(1);
    const PULSE: ItemType = ItemType(2);

    /// The stepper and the symbolic evaluator are independent
    /// implementations of the same semantics; on summarizable nets they
    /// must agree tick for tick. This cross-validates both.
    #[test]
    fn stepper_matches_symbolic_evaluator() {
        let mut lib = Library::new();
        let thr = throttle(&mut lib, ITEM, TOKEN, 3, 4);
        let store = demand_store(&mut lib, ITEM, TOKEN, PULSE, 4);

        for (id, inputs) in [
            (thr, vec![Counting::unit_rate()]),
            (store, vec![Counting::unit_rate().scale_floor(1, 2), Counting::unit_rate()]),
        ] {
            let mut ev = Evaluator::new(&lib);
            let symbolic = ev.evaluate(id, &inputs).unwrap();
            let mut stepper = Stepper::new(&lib, id).unwrap();
            let mut cum = vec![0u64; symbolic.len()];
            for t in 0..300u64 {
                let deltas: Vec<u64> = inputs
                    .iter()
                    .map(|c| c.eval(t) - if t == 0 { 0 } else { c.eval(t - 1) })
                    .collect();
                let out = stepper.step(&deltas).unwrap();
                for (c, d) in cum.iter_mut().zip(&out) {
                    *c += d;
                }
                for (p, c) in cum.iter().enumerate() {
                    assert_eq!(*c, symbolic[p].eval(t), "port {p} tick {t}");
                }
            }
        }
    }

    #[test]
    fn breeder_runs_exactly_where_the_summarizer_refuses() {
        // 3U -> 4U + pulse, self-loop: exponential, unsummarizable — but
        // steppable, exactly.
        let mut lib = Library::new();
        let mut b = NetBuilder::new();
        let n = b.recipe(
            Recipe::new(vec![3], vec![4, 1], 2),
            &[ITEM],
            &[ITEM, PULSE],
        );
        b.connect(n.output(0), n.input(0));
        b.marking(n.input(0), 3);
        let out = b.output(PULSE);
        b.connect(n.output(1), out);
        let id = lib.intern(b.build()).unwrap();

        let mut ev = Evaluator::new(&lib);
        assert_eq!(ev.evaluate(id, &[]), Err(EvalError::RateExplosion));

        let mut stepper = Stepper::new(&lib, id).unwrap();
        let mut cum = 0u64;
        let mut naive_k: Vec<u64> = Vec::new();
        for t in 0..60usize {
            cum += stepper.step(&[]).unwrap()[0];
            // Naive reference: k(t) = floor((3 + 4*k(t-2)) / 3).
            let prev = if t >= 2 { naive_k[t - 2] } else { 0 };
            naive_k.push((3 + 4 * prev) / 3);
            let expect: u64 = if t >= 2 { naive_k[t - 2] } else { 0 };
            assert_eq!(cum, expect, "pulse total at tick {t}");
        }
        assert!(cum > 1000, "should be growing fast by now: {cum}");
    }

    #[test]
    fn identical_instances_share_chunks() {
        let mut lib = Library::new();
        let mut b = NetBuilder::new();
        let n = b.recipe(Recipe::new(vec![3], vec![4, 1], 2), &[ITEM], &[ITEM, PULSE]);
        b.connect(n.output(0), n.input(0));
        b.marking(n.input(0), 3);
        let out = b.output(PULSE);
        b.connect(n.output(1), out);
        let id = lib.intern(b.build()).unwrap();

        let mut cache = ChunkCache::new();
        let empty_chunk: Vec<Vec<u64>> = vec![vec![]; CHUNK];

        let mut a = Stepper::new(&lib, id).unwrap();
        let out_a = cache.advance(0, &mut a, &empty_chunk).unwrap();
        assert_eq!(cache.misses, 1);

        let mut b2 = Stepper::new(&lib, id).unwrap();
        let out_b = cache.advance(0, &mut b2, &empty_chunk).unwrap();
        assert_eq!(cache.hits, 1, "identical instance should hit");
        assert_eq!(out_a, out_b);
        assert_eq!(a.state(), b2.state());
    }
}
