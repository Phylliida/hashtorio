//! Module inlining: rewrite a net into an equivalent recipe-only net.
//!
//! Flattening is pure graph surgery — semantics is preserved exactly (the
//! evaluator tests assert modular ≡ flattened). The evaluator uses it when a
//! feedback cycle passes *through* a module boundary, where the module can't
//! be summarized as a black box.
//!
//! Splicing rule at a module boundary: the outer wire feeding module input
//! `k` and the sub-net wire(s) mentioning `Input(k)` fuse into one wire —
//! sources union, markings add. Same for module outputs. Pass-through chains
//! (a module input wired straight to a module output) are chased; a chain
//! that closes on itself with no node in between is a [`NetError::PassThroughCycle`].

use std::collections::HashSet;

use crate::net::{Layout, Library, Net, NetError, NetId, Node, Source, Wire};

/// Recursively inline every module: the result contains only recipe nodes.
pub fn flatten(lib: &Library, id: NetId) -> Result<Net, NetError> {
    flatten_net(lib, lib.get(id).clone())
}

/// Like [`flatten`] for a net not (or not yet) in the library. Any modules
/// it references must be interned in `lib`.
pub fn flatten_net(lib: &Library, mut net: Net) -> Result<Net, NetError> {
    while let Some(m) = net.nodes.iter().position(|n| matches!(n, Node::Module(_))) {
        let Node::Module(mid) = net.nodes[m] else { unreachable!() };
        let sub = flatten(lib, mid)?; // recipe-only by induction
        net = inline(lib, &net, m, &sub)?;
    }
    Ok(net)
}

/// Contexts a raw source can be read in during splicing.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Side {
    Outer,
    Sub,
}

struct Inliner<'a> {
    outer: &'a Net,
    sub: &'a Net,
    m: usize,
    outer_layout: Layout,
    sub_layout: Layout,
    /// Wires currently being expanded, for pass-through cycle detection.
    expanding: HashSet<(Side, usize)>,
}

impl<'a> Inliner<'a> {
    /// Map an original outer node index to its index in the flattened net.
    fn remap_outer(&self, node: u32) -> u32 {
        debug_assert_ne!(node as usize, self.m);
        if (node as usize) < self.m {
            node
        } else {
            node - 1
        }
    }

    /// Map a sub-net node index to its index in the flattened net.
    fn remap_sub(&self, node: u32) -> u32 {
        (self.outer.nodes.len() - 1 + node as usize) as u32
    }

    /// Resolve one raw source into flattened-net sources plus extra marking
    /// contributed by any wires fused along the way.
    fn resolve(
        &mut self,
        side: Side,
        src: Source,
        out: &mut Vec<Source>,
        marking: &mut u64,
    ) -> Result<(), NetError> {
        match (side, src) {
            (Side::Outer, Source::Input(i)) => {
                out.push(Source::Input(i));
                Ok(())
            }
            (Side::Outer, Source::NodeOut { node, leg }) if node as usize != self.m => {
                out.push(Source::NodeOut { node: self.remap_outer(node), leg });
                Ok(())
            }
            (Side::Outer, Source::NodeOut { leg, .. }) => {
                // Module output leg: fuse in the sub-net wire that sinks at
                // sub output `leg`.
                let widx = self.sub_layout.output_wire(leg as usize);
                self.expand(Side::Sub, widx, out, marking)
            }
            (Side::Sub, Source::NodeOut { node, leg }) => {
                out.push(Source::NodeOut { node: self.remap_sub(node), leg });
                Ok(())
            }
            (Side::Sub, Source::Input(k)) => {
                // Sub-net input: fuse in the outer wire feeding module leg k.
                let widx = self.outer_layout.node_input_wire(self.m, k as usize);
                self.expand(Side::Outer, widx, out, marking)
            }
        }
    }

    fn expand(
        &mut self,
        side: Side,
        widx: usize,
        out: &mut Vec<Source>,
        marking: &mut u64,
    ) -> Result<(), NetError> {
        if !self.expanding.insert((side, widx)) {
            return Err(NetError::PassThroughCycle);
        }
        let (wire_marking, wire_sources) = {
            let net: &Net = match side {
                Side::Outer => self.outer,
                Side::Sub => self.sub,
            };
            let wire = &net.wires[widx];
            (wire.marking, wire.sources.clone())
        };
        *marking += wire_marking;
        for src in wire_sources {
            self.resolve(side, src, out, marking)?;
        }
        self.expanding.remove(&(side, widx));
        Ok(())
    }

    /// Resolve a whole wire (its own marking included) into a flattened wire.
    fn resolve_wire(&mut self, side: Side, widx: usize) -> Result<Wire, NetError> {
        let mut sources = Vec::new();
        let mut marking = 0u64;
        self.expand(side, widx, &mut sources, &mut marking)?;
        sources.sort_unstable();
        Ok(Wire { sources, marking })
    }
}

fn inline(lib: &Library, outer: &Net, m: usize, sub: &Net) -> Result<Net, NetError> {
    let mut inliner = Inliner {
        outer,
        sub,
        m,
        outer_layout: Layout::new(lib, outer),
        sub_layout: Layout::new(lib, sub),
        expanding: HashSet::new(),
    };

    let mut nodes: Vec<Node> = Vec::with_capacity(outer.nodes.len() - 1 + sub.nodes.len());
    for (i, n) in outer.nodes.iter().enumerate() {
        if i != m {
            nodes.push(n.clone());
        }
    }
    nodes.extend(sub.nodes.iter().cloned());

    // New wires in new sink order: surviving outer nodes' legs, then the
    // inlined sub nodes' legs, then outer net outputs.
    let mut wires: Vec<Wire> = Vec::new();
    for (i, node) in outer.nodes.iter().enumerate() {
        if i == m {
            continue;
        }
        let legs = lib.node_in_types(node).len();
        for leg in 0..legs {
            let widx = inliner.outer_layout.node_input_wire(i, leg);
            wires.push(inliner.resolve_wire(Side::Outer, widx)?);
        }
    }
    for (j, node) in sub.nodes.iter().enumerate() {
        let legs = lib.node_in_types(node).len();
        for leg in 0..legs {
            let widx = inliner.sub_layout.node_input_wire(j, leg);
            wires.push(inliner.resolve_wire(Side::Sub, widx)?);
        }
    }
    for o in 0..outer.outputs.len() {
        let widx = inliner.outer_layout.output_wire(o);
        wires.push(inliner.resolve_wire(Side::Outer, widx)?);
    }

    Ok(Net {
        inputs: outer.inputs.clone(),
        outputs: outer.outputs.clone(),
        nodes,
        wires,
    })
}
