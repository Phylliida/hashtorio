//! The evaluator: nets in, exact counting maps out.
//!
//! Architecture (see DESIGN.md):
//! - Nodes are grouped into strongly connected components and processed in
//!   dependency order.
//! - Acyclic recipe nodes evaluate symbolically with the M0 op algebra.
//! - Acyclic module nodes evaluate recursively, **memoized** on
//!   `(NetId, input countings)` — canonical `Counting` hashing makes the
//!   memo table a content-addressed behavior cache shared across every
//!   instance of a design. This is the HashLife move.
//! - Cyclic components (feedback) are solved by **guess-then-verify**: dense
//!   simulation proposes an ultimately periodic candidate, and the candidate
//!   is checked *symbolically* against the fixed-point equations with exact
//!   M0 operations. Because every cycle has total latency >= 1, the causal
//!   solution is unique, so any verified candidate is *the* behavior —
//!   soundness never depends on the guessing heuristic.
//! - A cycle passing through a module boundary can't treat the module as a
//!   black box; the evaluator flattens the net and retries (flattened nets
//!   are recipe-only, so this happens at most once per evaluation).

use std::collections::HashMap;

use crate::counting::{gcd, Counting};
use crate::flatten::flatten_net;
use crate::net::{Layout, Library, Net, NetError, NetId, Node, Source};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    Net(NetError),
    InputArity { expected: usize, got: usize },
    /// A feedback cycle with zero total latency: the fixed point is Zeno
    /// (infinitely many firings in one tick). Add latency somewhere.
    ZeroLatencyCycle,
    /// Firing counts exceeded the explosion cap: the net's long-run growth
    /// is superlinear (e.g. a self-amplifying breeder loop), so no
    /// linear-rate steady state exists.
    RateExplosion,
    /// No ultimately periodic steady state found within the horizon.
    NoPeriodicSteadyState { horizon: u64 },
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EvalError {}

/// Firing counts above this are treated as divergence, not steady state.
const EXPLOSION_CAP: u64 = 1 << 40;

pub struct Evaluator<'l> {
    lib: &'l Library,
    memo: HashMap<(NetId, Vec<Counting>), Vec<Counting>>,
    /// Maximum dense-simulation window for feedback solving.
    pub horizon: u64,
    /// Number of actual (non-memoized) net evaluations performed.
    pub interior_evals: u64,
}

/// The result of one net evaluation, including per-node detail for tracing.
struct NetRun {
    outputs: Vec<Counting>,
    /// Firing map per node (`None` for module nodes — their detail lives in
    /// their own evaluations).
    firings: Vec<Option<Counting>>,
    /// Output-leg countings per node.
    node_outs: Vec<Option<Vec<Counting>>>,
}

enum Flow {
    Done(NetRun),
    NeedsFlatten,
}

/// A fully-resolved evaluation of a recipe-only (flattened) net: everything
/// the conservation audit needs.
pub struct Trace {
    pub net: Net,
    pub inputs: Vec<Counting>,
    pub outputs: Vec<Counting>,
    /// Firing map per node.
    pub firings: Vec<Counting>,
    /// Output-leg countings per node.
    pub node_outs: Vec<Vec<Counting>>,
}

impl<'l> Evaluator<'l> {
    pub fn new(lib: &'l Library) -> Self {
        Evaluator { lib, memo: HashMap::new(), horizon: 1 << 14, interior_evals: 0 }
    }

    pub fn lib(&self) -> &'l Library {
        self.lib
    }

    /// Evaluate an interned net on the given input counting maps.
    pub fn evaluate(
        &mut self,
        id: NetId,
        inputs: &[Counting],
    ) -> Result<Vec<Counting>, EvalError> {
        let key = (id, inputs.to_vec());
        if let Some(hit) = self.memo.get(&key) {
            return Ok(hit.clone());
        }
        let net = self.lib.get(id).clone();
        self.interior_evals += 1;
        let outs = self.evaluate_net(&net, inputs)?;
        self.memo.insert(key, outs.clone());
        Ok(outs)
    }

    /// Evaluate a net that need not be interned (modules it references must
    /// be). Not memoized at this level; nested modules still are.
    pub fn evaluate_net(
        &mut self,
        net: &Net,
        inputs: &[Counting],
    ) -> Result<Vec<Counting>, EvalError> {
        match self.eval_net(net, inputs)? {
            Flow::Done(run) => Ok(run.outputs),
            Flow::NeedsFlatten => {
                let flat = flatten_net(self.lib, net.clone()).map_err(EvalError::Net)?;
                match self.eval_net(&flat, inputs)? {
                    Flow::Done(run) => Ok(run.outputs),
                    Flow::NeedsFlatten => unreachable!("flattened nets contain no modules"),
                }
            }
        }
    }

    /// Flatten a net and evaluate it with full per-node detail, for the
    /// conservation audit and other inspectors.
    pub fn trace_flattened(
        &mut self,
        id: NetId,
        inputs: &[Counting],
    ) -> Result<Trace, EvalError> {
        let flat = crate::flatten::flatten(self.lib, id).map_err(EvalError::Net)?;
        match self.eval_net(&flat, inputs)? {
            Flow::Done(run) => Ok(Trace {
                net: flat,
                inputs: inputs.to_vec(),
                outputs: run.outputs,
                firings: run
                    .firings
                    .into_iter()
                    .map(|f| f.expect("flattened nets have firings on every node"))
                    .collect(),
                node_outs: run
                    .node_outs
                    .into_iter()
                    .map(|o| o.expect("all nodes evaluated"))
                    .collect(),
            }),
            Flow::NeedsFlatten => unreachable!("flattened nets contain no modules"),
        }
    }

    fn eval_net(&mut self, net: &Net, inputs: &[Counting]) -> Result<Flow, EvalError> {
        if inputs.len() != net.inputs.len() {
            return Err(EvalError::InputArity {
                expected: net.inputs.len(),
                got: inputs.len(),
            });
        }
        let layout = Layout::new(self.lib, net);
        let nn = net.nodes.len();

        // Node dependency graph: an edge a -> b for every wire into b with a
        // source leg on a.
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); nn];
        {
            let mut w = 0usize;
            for (b, node) in net.nodes.iter().enumerate() {
                for _ in 0..self.lib.node_in_types(node).len() {
                    for src in &net.wires[w].sources {
                        if let Source::NodeOut { node: a, .. } = src {
                            adj[*a as usize].push(b);
                        }
                    }
                    w += 1;
                }
            }
        }

        let comps = tarjan_sccs(nn, &adj);
        let order = scc_topo_order(&comps, &adj, nn);
        let mut node_outs: Vec<Option<Vec<Counting>>> = vec![None; nn];
        let mut firings: Vec<Option<Counting>> = vec![None; nn];

        for &ci in &order {
            let comp = &comps[ci];
            let cyclic = comp.len() > 1 || adj[comp[0]].contains(&comp[0]);
            if !cyclic {
                let g = comp[0];
                let node = &net.nodes[g];
                let legs = self.lib.node_in_types(node).len();
                let ins: Vec<Counting> = (0..legs)
                    .map(|l| {
                        wire_counting(net, layout.node_input_wire(g, l), inputs, &node_outs)
                    })
                    .collect();
                let outs = match node {
                    Node::Recipe { recipe, .. } => {
                        let k = recipe.firings(&ins);
                        let fired = k.shift(recipe.latency);
                        let outs =
                            recipe.produce.iter().map(|&p| fired.scale_floor(p, 1)).collect();
                        firings[g] = Some(k);
                        outs
                    }
                    Node::Module(mid) => self.evaluate(*mid, &ins)?,
                };
                node_outs[g] = Some(outs);
            } else {
                if comp
                    .iter()
                    .any(|&g| matches!(net.nodes[g], Node::Module(_)))
                {
                    return Ok(Flow::NeedsFlatten);
                }
                solve_scc(net, &layout, comp, inputs, &mut node_outs, &mut firings, self.horizon)?;
            }
        }

        let outputs = (0..net.outputs.len())
            .map(|o| wire_counting(net, layout.output_wire(o), inputs, &node_outs))
            .collect();
        Ok(Flow::Done(NetRun { outputs, firings, node_outs }))
    }
}

/// The counting map on a wire: marking plus the merge of all sources.
fn wire_counting(
    net: &Net,
    widx: usize,
    inputs: &[Counting],
    node_outs: &[Option<Vec<Counting>>],
) -> Counting {
    let wire = &net.wires[widx];
    let mut acc = Counting::constant(wire.marking);
    for src in &wire.sources {
        let c = match src {
            Source::Input(i) => &inputs[*i as usize],
            Source::NodeOut { node, leg } => {
                &node_outs[*node as usize].as_ref().expect("topo order")[*leg as usize]
            }
        };
        acc = acc.add(c);
    }
    acc
}

// ---------------------------------------------------------------------------
// Feedback: guess-then-verify fixed points
// ---------------------------------------------------------------------------

/// One input leg of a node inside a cyclic component.
struct LegIn {
    /// Marking + all contributions from outside the component (exact).
    known: Counting,
    /// Contributions from inside: (local source node, amount/firing, latency).
    feeds: Vec<(usize, u64, u64)>,
    consume: u64,
}

struct SccNode {
    latency: u64,
    produce: Vec<u64>,
    legs: Vec<LegIn>,
}

fn solve_scc(
    net: &Net,
    layout: &Layout,
    comp: &[usize],
    inputs: &[Counting],
    node_outs: &mut [Option<Vec<Counting>>],
    firings: &mut [Option<Counting>],
    horizon: u64,
) -> Result<(), EvalError> {
    let local: HashMap<usize, usize> =
        comp.iter().enumerate().map(|(li, &g)| (g, li)).collect();

    // Gather the component's equations.
    let mut snodes: Vec<SccNode> = Vec::with_capacity(comp.len());
    for &g in comp {
        let Node::Recipe { recipe, .. } = &net.nodes[g] else {
            unreachable!("cyclic components with modules are flattened first")
        };
        let mut legs = Vec::with_capacity(recipe.consume.len());
        for (leg_i, &c) in recipe.consume.iter().enumerate() {
            let wire = &net.wires[layout.node_input_wire(g, leg_i)];
            let mut known = Counting::constant(wire.marking);
            let mut feeds = Vec::new();
            for src in &wire.sources {
                match src {
                    Source::Input(i) => known = known.add(&inputs[*i as usize]),
                    Source::NodeOut { node: a, leg } => {
                        if let Some(&la) = local.get(&(*a as usize)) {
                            let Node::Recipe { recipe: ra, .. } = &net.nodes[*a as usize]
                            else {
                                unreachable!()
                            };
                            feeds.push((la, ra.produce[*leg as usize], ra.latency));
                        } else {
                            known = known
                                .add(&node_outs[*a as usize].as_ref().expect("topo order")
                                    [*leg as usize]);
                        }
                    }
                }
            }
            legs.push(LegIn { known, feeds, consume: c });
        }
        snodes.push(SccNode {
            latency: recipe.latency,
            produce: recipe.produce.clone(),
            legs,
        });
    }

    // Within one tick, values propagate along zero-latency producers; that
    // subgraph must be acyclic or the fixed point is Zeno.
    let intra_order = zero_latency_topo(&snodes).ok_or(EvalError::ZeroLatencyCycle)?;

    // The steady-state period must be compatible with the known parts'
    // periods; simulate at least past their transients.
    let tmin = snodes
        .iter()
        .flat_map(|n| n.legs.iter().map(|l| l.known.transient_len()))
        .max()
        .unwrap_or(0);
    let l = snodes
        .iter()
        .flat_map(|n| n.legs.iter().map(|l| l.known.period()))
        .fold(1u64, |acc, p| lcm_capped(acc, p, horizon));

    let mut t_sim = (4 * (tmin as u64 + l) + 64).clamp(16, horizon);
    loop {
        if let Some(cands) = attempt(&snodes, &intra_order, t_sim, tmin, l)? {
            for (li, &g) in comp.iter().enumerate() {
                let sn = &snodes[li];
                let fired = cands[li].shift(sn.latency);
                node_outs[g] =
                    Some(sn.produce.iter().map(|&p| fired.scale_floor(p, 1)).collect());
                firings[g] = Some(cands[li].clone());
            }
            return Ok(());
        }
        if t_sim >= horizon {
            return Err(EvalError::NoPeriodicSteadyState { horizon });
        }
        t_sim = (t_sim * 2).min(horizon);
    }
}

/// Topological order of component nodes along zero-latency feed edges, or
/// `None` if they cycle.
fn zero_latency_topo(snodes: &[SccNode]) -> Option<Vec<usize>> {
    let m = snodes.len();
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); m];
    let mut indeg = vec![0usize; m];
    for (li, sn) in snodes.iter().enumerate() {
        for leg in &sn.legs {
            for &(la, _, lat) in &leg.feeds {
                if lat == 0 {
                    edges[la].push(li);
                    indeg[li] += 1;
                }
            }
        }
    }
    let mut queue: Vec<usize> = (0..m).filter(|&i| indeg[i] == 0).collect();
    let mut order = Vec::with_capacity(m);
    while let Some(v) = queue.pop() {
        order.push(v);
        for &w in &edges[v] {
            indeg[w] -= 1;
            if indeg[w] == 0 {
                queue.push(w);
            }
        }
    }
    (order.len() == m).then_some(order)
}

/// Simulate `t_sim` ticks densely, look for a repeating delta pattern, and
/// verify any candidate symbolically. `Ok(None)` = no verified candidate at
/// this horizon.
fn attempt(
    snodes: &[SccNode],
    intra_order: &[usize],
    t_sim: u64,
    tmin: usize,
    l: u64,
) -> Result<Option<Vec<Counting>>, EvalError> {
    let m = snodes.len();
    let t_sim = t_sim as usize;

    // Dense simulation of firing counts.
    let mut k: Vec<Vec<u64>> = vec![Vec::with_capacity(t_sim); m];
    for t in 0..t_sim {
        for &li in intra_order {
            let sn = &snodes[li];
            let mut kk = u64::MAX;
            for leg in &sn.legs {
                let mut v = leg.known.eval(t as u64);
                for &(la, amt, lat) in &leg.feeds {
                    if t as u64 >= lat {
                        // For lat == 0 the source is earlier in intra_order,
                        // so k[la][t] is already computed.
                        let kv = k[la][t - lat as usize];
                        v = v
                            .checked_add(amt.checked_mul(kv).ok_or(EvalError::RateExplosion)?)
                            .ok_or(EvalError::RateExplosion)?;
                    }
                }
                kk = kk.min(v / leg.consume);
            }
            if kk > EXPLOSION_CAP {
                return Err(EvalError::RateExplosion);
            }
            k[li].push(kk);
        }
    }

    // Look for a period: smallest multiple of `l` whose per-tick firing
    // deltas repeat over a verified suffix at least one period long.
    let t0_floor = tmin.max(1);
    let dk = |li: usize, t: usize| k[li][t] - k[li][t - 1];
    let mut pm = 1u64;
    loop {
        let p = pm * l;
        let pu = p as usize;
        if t0_floor + 2 * pu > t_sim {
            return Ok(None);
        }
        // Longest suffix [lo, t_sim - p) on which deltas repeat with lag p.
        let mut lo = t_sim - pu;
        while lo > t0_floor && (0..m).all(|li| dk(li, lo - 1 + pu) == dk(li, lo - 1)) {
            lo -= 1;
        }
        if t_sim - pu - lo >= pu {
            let t0 = lo;
            let cands: Option<Vec<Counting>> = (0..m)
                .map(|li| {
                    let slope = k[li][t0 + pu] - k[li][t0];
                    Counting::try_from_parts(k[li][..t0 + pu].to_vec(), t0, p, slope)
                })
                .collect();
            if let Some(cands) = cands {
                if verify(snodes, &cands) {
                    return Ok(Some(cands));
                }
            }
        }
        pm += 1;
    }
}

/// Check the fixed-point equations symbolically: for every component node,
/// the candidate firing map must equal the firings computed from the wire
/// countings that the candidates themselves induce. Exact M0 algebra —
/// soundness of the whole feedback solver lives here.
fn verify(snodes: &[SccNode], cands: &[Counting]) -> bool {
    for (li, sn) in snodes.iter().enumerate() {
        let mut firings: Option<Counting> = None;
        for leg in &sn.legs {
            let mut wire = leg.known.clone();
            for &(la, amt, lat) in &leg.feeds {
                wire = wire.add(&cands[la].shift(lat).scale_floor(amt, 1));
            }
            let f = wire.scale_floor(1, leg.consume);
            firings = Some(match firings {
                None => f,
                Some(acc) => acc.min(&f),
            });
        }
        if firings.expect("recipes have >= 1 input leg") != cands[li] {
            return false;
        }
    }
    true
}

fn lcm_capped(a: u64, b: u64, cap: u64) -> u64 {
    let g = gcd(a, b);
    let l = (a / g) as u128 * b as u128;
    if l > cap as u128 {
        cap.saturating_add(1)
    } else {
        l as u64
    }
}

// ---------------------------------------------------------------------------
// Graph utilities
// ---------------------------------------------------------------------------

fn tarjan_sccs(n: usize, adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct T<'a> {
        adj: &'a [Vec<usize>],
        index: Vec<i64>,
        low: Vec<i64>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        next: i64,
        out: Vec<Vec<usize>>,
    }
    impl T<'_> {
        fn strongconnect(&mut self, v: usize) {
            self.index[v] = self.next;
            self.low[v] = self.next;
            self.next += 1;
            self.stack.push(v);
            self.on_stack[v] = true;
            for i in 0..self.adj[v].len() {
                let w = self.adj[v][i];
                if self.index[w] < 0 {
                    self.strongconnect(w);
                    self.low[v] = self.low[v].min(self.low[w]);
                } else if self.on_stack[w] {
                    self.low[v] = self.low[v].min(self.index[w]);
                }
            }
            if self.low[v] == self.index[v] {
                let mut comp = Vec::new();
                loop {
                    let w = self.stack.pop().unwrap();
                    self.on_stack[w] = false;
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                self.out.push(comp);
            }
        }
    }
    let mut t = T {
        adj,
        index: vec![-1; n],
        low: vec![-1; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };
    for v in 0..n {
        if t.index[v] < 0 {
            t.strongconnect(v);
        }
    }
    t.out
}

/// Dependency order over the condensation (sources first).
fn scc_topo_order(comps: &[Vec<usize>], adj: &[Vec<usize>], n: usize) -> Vec<usize> {
    let mut comp_of = vec![0usize; n];
    for (ci, comp) in comps.iter().enumerate() {
        for &v in comp {
            comp_of[v] = ci;
        }
    }
    let nc = comps.len();
    let mut cadj: Vec<Vec<usize>> = vec![Vec::new(); nc];
    let mut indeg = vec![0usize; nc];
    for v in 0..n {
        for &w in &adj[v] {
            let (a, b) = (comp_of[v], comp_of[w]);
            if a != b {
                cadj[a].push(b);
                indeg[b] += 1;
            }
        }
    }
    let mut queue: Vec<usize> = (0..nc).filter(|&c| indeg[c] == 0).collect();
    let mut order = Vec::with_capacity(nc);
    while let Some(c) = queue.pop() {
        order.push(c);
        for &d in &cadj[c] {
            indeg[d] -= 1;
            if indeg[d] == 0 {
                queue.push(d);
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flatten::flatten;
    use crate::net::{ItemType, NetBuilder};
    use crate::recipe::Recipe;

    const IRON: ItemType = ItemType(0);
    const GEAR: ItemType = ItemType(1);

    fn gear_module(lib: &mut Library) -> NetId {
        let mut b = NetBuilder::new();
        let iron = b.input(IRON);
        let n = b.recipe(Recipe::new(vec![2], vec![1], 3), &[IRON], &[GEAR]);
        let out = b.output(GEAR);
        b.connect(iron, n.input(0));
        b.connect(n.output(0), out);
        lib.intern(b.build()).unwrap()
    }

    #[test]
    fn feedforward_matches_direct_algebra() {
        let mut lib = Library::new();
        let id = gear_module(&mut lib);
        let mut ev = Evaluator::new(&lib);
        let outs = ev.evaluate(id, &[Counting::unit_rate()]).unwrap();
        let direct = Recipe::new(vec![2], vec![1], 3).apply(&[Counting::unit_rate()]);
        assert_eq!(outs, direct);
    }

    #[test]
    fn module_summaries_are_memoized() {
        let mut lib = Library::new();
        let gear = gear_module(&mut lib);
        // Two instances of the same module on two identical input streams.
        let mut b = NetBuilder::new();
        let i1 = b.input(IRON);
        let i2 = b.input(IRON);
        let m1 = b.module(&lib, gear);
        let m2 = b.module(&lib, gear);
        let o1 = b.output(GEAR);
        let o2 = b.output(GEAR);
        b.connect(i1, m1.input(0));
        b.connect(i2, m2.input(0));
        b.connect(m1.output(0), o1);
        b.connect(m2.output(0), o2);
        let parent = lib.intern(b.build()).unwrap();

        let mut ev = Evaluator::new(&lib);
        let u = Counting::unit_rate();
        let outs = ev.evaluate(parent, &[u.clone(), u.clone()]).unwrap();
        assert_eq!(outs[0], outs[1]);
        // Parent evaluated once, gear interior evaluated once; the second
        // instance is a memo hit.
        assert_eq!(ev.interior_evals, 2);
        // Re-evaluating the parent is a memo hit too.
        ev.evaluate(parent, &[u.clone(), u]).unwrap();
        assert_eq!(ev.interior_evals, 2);
    }

    #[test]
    fn flattened_equals_modular() {
        let mut lib = Library::new();
        let gear = gear_module(&mut lib);
        // gears -> gears-of-gears chain through two module instances.
        let mut b = NetBuilder::new();
        let iron = b.input(IRON);
        let m1 = b.module(&lib, gear);
        let cast = b.recipe(Recipe::new(vec![1], vec![1], 0), &[GEAR], &[IRON]);
        let m2 = b.module(&lib, gear);
        let out = b.output(GEAR);
        b.connect(iron, m1.input(0));
        b.connect(m1.output(0), cast.input(0));
        b.connect(cast.output(0), m2.input(0));
        b.connect(m2.output(0), out);
        let parent = lib.intern(b.build()).unwrap();

        let flat = flatten(&lib, parent).unwrap();
        assert!(flat.nodes.iter().all(|n| matches!(n, Node::Recipe { .. })));

        let mut ev = Evaluator::new(&lib);
        let modular = ev.evaluate(parent, &[Counting::unit_rate()]).unwrap();
        let flattened = ev.evaluate_net(&flat, &[Counting::unit_rate()]).unwrap();
        assert_eq!(modular, flattened);
        assert_eq!(modular[0].rate(), (1, 4));
    }
}
