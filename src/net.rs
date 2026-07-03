//! The term language: typed wiring nets and the hash-consed blueprint library.
//!
//! A [`Net`] is a bundle of typed input/output ports, a list of nodes
//! (primitive recipes or nested modules), and one [`Wire`] per *sink*.
//! Wiring discipline (the linearity of items, structurally enforced):
//!
//! - every sink (node input leg, net output) has exactly one wire;
//! - a wire may have **many sources** — merging is free and deterministic
//!   (pointwise sum of counts) — but each source feeds **at most one** wire:
//!   there is no implicit copying of items. Splitting is a recipe.
//! - a wire may carry an initial **marking** (preloaded items) — the kernel's
//!   only state primitive.
//!
//! Modules are *not* primitives: a [`Node::Module`] is a reference into the
//! [`Library`], which interns nets by structural identity. Because a `Net`
//! contains `NetId`s that were themselves interned, structural hashing gives
//! a Merkle-DAG content address for free, and `NetId` equality means
//! "identical design all the way down". Canonicalization is per-wire only
//! (sources are sorted); nets that differ by node ordering do not dedup —
//! cheap and good enough, per DESIGN.md.

use std::collections::{HashMap, HashSet};

use crate::recipe::Recipe;

/// An item type tag. A registry with names can live above the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemType(pub u32);

/// Content-addressed reference to an interned [`Net`]. Only obtainable from
/// [`Library::intern`], so a `NetId` always refers to a validated net, and
/// the reference graph is acyclic by construction (a net can only reference
/// nets interned before it — recursion in hashtorio is containment, not
/// self-reference).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NetId(u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    Recipe {
        recipe: Recipe,
        in_types: Vec<ItemType>,
        out_types: Vec<ItemType>,
    },
    Module(NetId),
}

/// A producer endpoint: a net input port or a node output leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Input(u32),
    NodeOut { node: u32, leg: u32 },
}

/// The wire feeding one sink: merged sources plus an initial marking.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Wire {
    pub sources: Vec<Source>,
    pub marking: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Net {
    pub inputs: Vec<ItemType>,
    pub outputs: Vec<ItemType>,
    pub nodes: Vec<Node>,
    /// One wire per sink, in canonical sink order: every node input leg
    /// (nodes in order, legs in order), then every net output.
    pub wires: Vec<Wire>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    WireCountMismatch { expected: usize, got: usize },
    RecipeArity { node: usize },
    BadSource { wire: usize, source: Source },
    SourceUsedTwice(Source),
    TypeMismatch { wire: usize, source: Source },
    /// A cycle made only of wires (through module boundaries), touching no
    /// node — items circulating with nothing ever consuming them.
    PassThroughCycle,
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for NetError {}

/// Interning store for nets. The Merkle-DAG blueprint library.
#[derive(Debug, Default)]
pub struct Library {
    nets: Vec<Net>,
    index: HashMap<Net, NetId>,
}

impl Library {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: NetId) -> &Net {
        &self.nets[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.nets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }

    /// Validate, canonicalize, and dedup a net. Structurally identical nets
    /// (after per-wire source sorting) return the same `NetId`.
    pub fn intern(&mut self, mut net: Net) -> Result<NetId, NetError> {
        for w in &mut net.wires {
            w.sources.sort_unstable();
        }
        self.validate(&net)?;
        if let Some(&id) = self.index.get(&net) {
            return Ok(id);
        }
        let id = NetId(self.nets.len() as u32);
        self.index.insert(net.clone(), id);
        self.nets.push(net);
        Ok(id)
    }

    pub fn node_in_types(&self, node: &Node) -> Vec<ItemType> {
        match node {
            Node::Recipe { in_types, .. } => in_types.clone(),
            Node::Module(id) => self.get(*id).inputs.clone(),
        }
    }

    pub fn node_out_types(&self, node: &Node) -> Vec<ItemType> {
        match node {
            Node::Recipe { out_types, .. } => out_types.clone(),
            Node::Module(id) => self.get(*id).outputs.clone(),
        }
    }

    fn validate(&self, net: &Net) -> Result<(), NetError> {
        for (idx, node) in net.nodes.iter().enumerate() {
            if let Node::Recipe { recipe, in_types, out_types } = node {
                if in_types.len() != recipe.consume.len()
                    || out_types.len() != recipe.produce.len()
                {
                    return Err(NetError::RecipeArity { node: idx });
                }
            }
            // Node::Module ids are unforgeable, so they are always in range.
        }
        // Sink enumeration: node input legs in order, then net outputs.
        let mut sink_types: Vec<ItemType> = Vec::new();
        for node in &net.nodes {
            sink_types.extend(self.node_in_types(node));
        }
        sink_types.extend(net.outputs.iter().copied());
        if net.wires.len() != sink_types.len() {
            return Err(NetError::WireCountMismatch {
                expected: sink_types.len(),
                got: net.wires.len(),
            });
        }
        let out_types: Vec<Vec<ItemType>> =
            net.nodes.iter().map(|n| self.node_out_types(n)).collect();
        let mut used: HashSet<Source> = HashSet::new();
        for (widx, wire) in net.wires.iter().enumerate() {
            let sink_ty = sink_types[widx];
            for &src in &wire.sources {
                let src_ty = match src {
                    Source::Input(i) => *net
                        .inputs
                        .get(i as usize)
                        .ok_or(NetError::BadSource { wire: widx, source: src })?,
                    Source::NodeOut { node, leg } => *out_types
                        .get(node as usize)
                        .and_then(|legs| legs.get(leg as usize))
                        .ok_or(NetError::BadSource { wire: widx, source: src })?,
                };
                if src_ty != sink_ty {
                    return Err(NetError::TypeMismatch { wire: widx, source: src });
                }
                if !used.insert(src) {
                    return Err(NetError::SourceUsedTwice(src));
                }
            }
        }
        Ok(())
    }
}

/// Sink-index layout of a net: node input legs first, then net outputs.
pub(crate) struct Layout {
    /// `in_offset[n]` = wire index of node `n`'s first input leg.
    pub in_offset: Vec<usize>,
    pub total_node_inputs: usize,
}

impl Layout {
    pub fn new(lib: &Library, net: &Net) -> Self {
        let mut in_offset = Vec::with_capacity(net.nodes.len());
        let mut acc = 0usize;
        for node in &net.nodes {
            in_offset.push(acc);
            acc += lib.node_in_types(node).len();
        }
        Layout { in_offset, total_node_inputs: acc }
    }

    pub fn node_input_wire(&self, node: usize, leg: usize) -> usize {
        self.in_offset[node] + leg
    }

    pub fn output_wire(&self, output: usize) -> usize {
        self.total_node_inputs + output
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeHandle(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceRef(Source);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SinkInner {
    NodeIn { node: u32, leg: u32 },
    Output(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SinkRef(SinkInner);

impl NodeHandle {
    pub fn input(self, leg: u32) -> SinkRef {
        SinkRef(SinkInner::NodeIn { node: self.0, leg })
    }
    pub fn output(self, leg: u32) -> SourceRef {
        SourceRef(Source::NodeOut { node: self.0, leg })
    }
}

/// Ergonomic net construction. Structural/type validation happens at
/// [`Library::intern`]; the builder only tracks shape.
#[derive(Debug, Default)]
pub struct NetBuilder {
    inputs: Vec<ItemType>,
    outputs: Vec<ItemType>,
    nodes: Vec<Node>,
    node_in_counts: Vec<usize>,
    conns: Vec<(SourceRef, SinkRef)>,
    markings: Vec<(SinkRef, u64)>,
}

impl NetBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn input(&mut self, ty: ItemType) -> SourceRef {
        self.inputs.push(ty);
        SourceRef(Source::Input(self.inputs.len() as u32 - 1))
    }

    pub fn output(&mut self, ty: ItemType) -> SinkRef {
        self.outputs.push(ty);
        SinkRef(SinkInner::Output(self.outputs.len() as u32 - 1))
    }

    pub fn recipe(
        &mut self,
        recipe: Recipe,
        in_types: &[ItemType],
        out_types: &[ItemType],
    ) -> NodeHandle {
        assert_eq!(in_types.len(), recipe.consume.len(), "input leg arity");
        assert_eq!(out_types.len(), recipe.produce.len(), "output leg arity");
        self.node_in_counts.push(in_types.len());
        self.nodes.push(Node::Recipe {
            recipe,
            in_types: in_types.to_vec(),
            out_types: out_types.to_vec(),
        });
        NodeHandle(self.nodes.len() as u32 - 1)
    }

    pub fn module(&mut self, lib: &Library, id: NetId) -> NodeHandle {
        self.node_in_counts.push(lib.get(id).inputs.len());
        self.nodes.push(Node::Module(id));
        NodeHandle(self.nodes.len() as u32 - 1)
    }

    /// Connect a source to a sink. Connecting several sources to one sink
    /// merges them (pointwise sum of counts).
    pub fn connect(&mut self, from: SourceRef, to: SinkRef) {
        self.conns.push((from, to));
    }

    /// Preload the wire at `sink` with `marking` initial items.
    pub fn marking(&mut self, sink: SinkRef, marking: u64) {
        self.markings.push((sink, marking));
    }

    pub fn build(self) -> Net {
        let total: usize = self.node_in_counts.iter().sum::<usize>() + self.outputs.len();
        let mut offsets = Vec::with_capacity(self.node_in_counts.len());
        let mut acc = 0usize;
        for c in &self.node_in_counts {
            offsets.push(acc);
            acc += c;
        }
        let sink_index = |s: SinkRef| -> usize {
            match s.0 {
                SinkInner::NodeIn { node, leg } => offsets[node as usize] + leg as usize,
                SinkInner::Output(i) => acc + i as usize,
            }
        };
        let mut wires: Vec<Wire> =
            (0..total).map(|_| Wire { sources: Vec::new(), marking: 0 }).collect();
        for (from, to) in &self.conns {
            wires[sink_index(*to)].sources.push(from.0);
        }
        for (sink, m) in &self.markings {
            wires[sink_index(*sink)].marking += m;
        }
        Net {
            inputs: self.inputs,
            outputs: self.outputs,
            nodes: self.nodes,
            wires,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IRON: ItemType = ItemType(0);
    const GEAR: ItemType = ItemType(1);

    fn gear_net() -> Net {
        let mut b = NetBuilder::new();
        let iron = b.input(IRON);
        let n = b.recipe(Recipe::new(vec![2], vec![1], 3), &[IRON], &[GEAR]);
        let out = b.output(GEAR);
        b.connect(iron, n.input(0));
        b.connect(n.output(0), out);
        b.build()
    }

    #[test]
    fn intern_dedups_identical_nets() {
        let mut lib = Library::new();
        let a = lib.intern(gear_net()).unwrap();
        let b = lib.intern(gear_net()).unwrap();
        assert_eq!(a, b);
        assert_eq!(lib.len(), 1);
        // A marking difference is a different design.
        let mut net = gear_net();
        net.wires[0].marking = 7;
        let c = lib.intern(net).unwrap();
        assert_ne!(a, c);
        assert_eq!(lib.len(), 2);
    }

    #[test]
    fn source_order_is_canonicalized() {
        let build = |flip: bool| {
            let mut b = NetBuilder::new();
            let x = b.input(IRON);
            let y = b.input(IRON);
            let out = b.output(IRON);
            if flip {
                b.connect(y, out);
                b.connect(x, out);
            } else {
                b.connect(x, out);
                b.connect(y, out);
            }
            b.build()
        };
        let mut lib = Library::new();
        let a = lib.intern(build(false)).unwrap();
        let b = lib.intern(build(true)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn type_mismatch_rejected() {
        let mut b = NetBuilder::new();
        let iron = b.input(IRON);
        let out = b.output(GEAR);
        b.connect(iron, out);
        let mut lib = Library::new();
        assert!(matches!(
            lib.intern(b.build()),
            Err(NetError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn double_use_of_source_rejected() {
        // One iron input wired to two sinks: implicit copying, forbidden.
        let mut b = NetBuilder::new();
        let iron = b.input(IRON);
        let o1 = b.output(IRON);
        let o2 = b.output(IRON);
        b.connect(iron, o1);
        b.connect(iron, o2);
        let mut lib = Library::new();
        assert!(matches!(
            lib.intern(b.build()),
            Err(NetError::SourceUsedTwice(_))
        ));
    }

    #[test]
    fn modules_intern_as_merkle_dag() {
        let mut lib = Library::new();
        let gear = lib.intern(gear_net()).unwrap();
        let parent = |lib: &Library| {
            let mut b = NetBuilder::new();
            let iron = b.input(IRON);
            let m = b.module(lib, gear);
            let out = b.output(GEAR);
            b.connect(iron, m.input(0));
            b.connect(m.output(0), out);
            b.build()
        };
        let p1 = lib.intern(parent(&lib)).unwrap();
        let p2 = lib.intern(parent(&lib)).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(lib.len(), 2);
    }
}
