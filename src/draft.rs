//! The editable blueprint model: what the GUI editor manipulates and what
//! compiles into an interned net.
//!
//! A [`Draft`] is deliberately dumber than a [`crate::net::Net`]: flat lists
//! of inputs, outputs, nodes, wires, and markings, in construction order, so
//! that editor positions map 1:1 onto compiled indices. `build()` validates
//! with *friendly* errors — the compile button talks to a player, and the
//! refusals are the game teaching its own rules ("items can't be copied",
//! "add latency somewhere in that loop").

use crate::counting::Counting;
use crate::net::{ItemType, Library, NetBuilder, NetError, NetId};
use crate::recipe::Recipe;
use crate::structure::{StructLib, ANY};

#[derive(Debug, Clone)]
pub struct DraftInput {
    pub ty: ItemType,
    pub label: String,
    /// Supply rate as items per ticks: `num` items every `den` ticks.
    pub rate: (u64, u64),
}

#[derive(Debug, Clone)]
pub struct DraftOutput {
    pub ty: ItemType,
    pub label: String,
}

#[derive(Debug, Clone)]
pub enum DraftNode {
    Recipe {
        label: String,
        consume: Vec<(ItemType, u64)>,
        produce: Vec<(ItemType, u64)>,
        latency: u64,
    },
    Priority {
        label: String,
        item: ItemType,
        token: ItemType,
    },
    /// A sealed sub-factory, nested **by value**: the editor's whole
    /// document stays one blob. At compile time the sub-draft interns
    /// first, so two identical sealed modules dedup to the same `NetId` —
    /// by-value in the editor, content-addressed in the engine.
    Module {
        label: String,
        draft: Box<Draft>,
    },
    /// A polymorphic constructor machine: its concrete types are inferred
    /// from what you wire into it. The wiring graph is the expression tree
    /// of the artifact it builds.
    Builder {
        label: String,
        op: BuildOp,
        latency: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildOp {
    /// `1·x + 1·y -> 1·weld(x, y @ (dx,dy))` — refuses if parts collide.
    Weld { dx: i32, dy: i32 },
    /// `1·x -> 1·rot(x)` — quarter-turn.
    Rot,
    /// `2·x -> 1·x + 1·x` — round-robin splitter for any type.
    Split,
    /// `1·x -> 1·x` — a belt: pure latency for any type.
    Belt,
}

impl BuildOp {
    pub fn in_arity(&self) -> usize {
        match self {
            BuildOp::Weld { .. } => 2,
            _ => 1,
        }
    }

    pub fn out_arity(&self) -> usize {
        match self {
            BuildOp::Split => 2,
            _ => 1,
        }
    }
}

impl DraftNode {
    pub fn label(&self) -> &str {
        match self {
            DraftNode::Recipe { label, .. }
            | DraftNode::Priority { label, .. }
            | DraftNode::Module { label, .. }
            | DraftNode::Builder { label, .. } => label,
        }
    }

    /// Input leg types; [`ANY`] on builder legs until inference resolves.
    pub fn in_types(&self) -> Vec<ItemType> {
        match self {
            DraftNode::Recipe { consume, .. } => consume.iter().map(|&(t, _)| t).collect(),
            DraftNode::Priority { item, token, .. } => vec![*item, *token],
            DraftNode::Module { draft, .. } => draft.inputs.iter().map(|i| i.ty).collect(),
            DraftNode::Builder { op, .. } => vec![ANY; op.in_arity()],
        }
    }

    /// Output leg types; [`ANY`] on builder legs until inference resolves.
    pub fn out_types(&self) -> Vec<ItemType> {
        match self {
            DraftNode::Recipe { produce, .. } => produce.iter().map(|&(t, _)| t).collect(),
            DraftNode::Priority { item, .. } => vec![*item, *item],
            DraftNode::Module { draft, .. } => draft.outputs.iter().map(|o| o.ty).collect(),
            DraftNode::Builder { op, .. } => vec![ANY; op.out_arity()],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftFrom {
    Input(usize),
    Node(usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftTo {
    Node(usize, usize),
    Output(usize),
}

#[derive(Debug, Clone, Default)]
pub struct Draft {
    /// The item palette: (id, display name).
    pub types: Vec<(ItemType, String)>,
    pub inputs: Vec<DraftInput>,
    pub outputs: Vec<DraftOutput>,
    pub nodes: Vec<DraftNode>,
    pub wires: Vec<(DraftFrom, DraftTo)>,
    pub markings: Vec<(DraftTo, u64)>,
}

impl Draft {
    pub fn type_name(&self, ty: ItemType) -> &str {
        self.types
            .iter()
            .find(|(t, _)| *t == ty)
            .map(|(_, n)| n.as_str())
            .unwrap_or("?")
    }

    /// The sink's item type (for edge coloring and type checks).
    pub fn sink_type(&self, to: DraftTo) -> Option<ItemType> {
        match to {
            DraftTo::Node(n, leg) => self.nodes.get(n)?.in_types().get(leg).copied(),
            DraftTo::Output(o) => self.outputs.get(o).map(|out| out.ty),
        }
    }

    fn check(&self) -> Result<(), String> {
        for (i, node) in self.nodes.iter().enumerate() {
            if let DraftNode::Recipe { consume, produce, .. } = node {
                if consume.is_empty() {
                    return Err(format!(
                        "machine {i} ({}) needs at least one input",
                        node.label()
                    ));
                }
                if consume.iter().chain(produce).any(|&(_, amt)| amt == 0) {
                    return Err(format!(
                        "machine {i} ({}) has a zero amount in its recipe",
                        node.label()
                    ));
                }
            }
        }
        let from_ok = |f: &DraftFrom| match *f {
            DraftFrom::Input(i) => i < self.inputs.len(),
            DraftFrom::Node(n, leg) => {
                self.nodes.get(n).is_some_and(|nd| leg < nd.out_types().len())
            }
        };
        let to_ok = |t: &DraftTo| match *t {
            DraftTo::Output(o) => o < self.outputs.len(),
            DraftTo::Node(n, leg) => {
                self.nodes.get(n).is_some_and(|nd| leg < nd.in_types().len())
            }
        };
        for (f, t) in &self.wires {
            if !from_ok(f) || !to_ok(t) {
                return Err("a wire points at something that no longer exists".into());
            }
        }
        for (t, _) in &self.markings {
            if !to_ok(t) {
                return Err("a marking sits on a port that no longer exists".into());
            }
        }
        for input in &self.inputs {
            if input.rate.1 == 0 {
                return Err(format!("source '{}' has a zero-tick period", input.label));
            }
        }
        Ok(())
    }

    /// Forward type inference: resolve each builder's concrete leg types
    /// from what feeds it. The wiring graph is the artifact's expression
    /// tree; this walks it.
    #[allow(clippy::type_complexity)]
    fn infer(
        &self,
        structs: &mut StructLib,
    ) -> Result<Vec<(Vec<ItemType>, Vec<ItemType>)>, String> {
        let mut resolved: Vec<Option<(Vec<ItemType>, Vec<ItemType>)>> = self
            .nodes
            .iter()
            .map(|node| match node {
                DraftNode::Builder { .. } => None,
                other => Some((other.in_types(), other.out_types())),
            })
            .collect();
        loop {
            let mut progress = false;
            let mut all_done = true;
            for n in 0..self.nodes.len() {
                if resolved[n].is_some() {
                    continue;
                }
                let DraftNode::Builder { label, op, .. } = &self.nodes[n] else {
                    unreachable!()
                };
                let mut leg_tys: Vec<Option<ItemType>> = vec![None; op.in_arity()];
                let mut wired = vec![false; op.in_arity()];
                let mut blocked = false;
                for (from, to) in &self.wires {
                    let DraftTo::Node(tn, leg) = *to else { continue };
                    if tn != n {
                        continue;
                    }
                    wired[leg] = true;
                    let src_ty = match *from {
                        DraftFrom::Input(i) => Some(self.inputs[i].ty),
                        DraftFrom::Node(a, aleg) => {
                            resolved[a].as_ref().map(|(_, outs)| outs[aleg])
                        }
                    };
                    match (src_ty, leg_tys[leg]) {
                        (None, _) => blocked = true, // upstream not resolved yet
                        (Some(t), None) => leg_tys[leg] = Some(t),
                        (Some(t), Some(seen)) if t != seen => {
                            return Err(format!(
                                "two different structures merge into '{label}' on one \
                                 port — keep each wire to a single shape"
                            ));
                        }
                        _ => {}
                    }
                }
                if let Some(unwired) = wired.iter().position(|w| !w) {
                    return Err(format!(
                        "wire input {unwired} of builder '{label}' — its type comes \
                         from what you feed it"
                    ));
                }
                if blocked {
                    all_done = false;
                    continue;
                }
                let ins: Vec<ItemType> =
                    leg_tys.into_iter().map(|t| t.expect("all legs wired")).collect();
                let outs = match op {
                    BuildOp::Weld { dx, dy } => vec![structs
                        .weld(ins[0], ins[1], *dx, *dy)
                        .map_err(|e| format!("'{label}': {e}"))?],
                    BuildOp::Rot => {
                        vec![structs.rot(ins[0]).map_err(|e| format!("'{label}': {e}"))?]
                    }
                    BuildOp::Split => vec![ins[0], ins[0]],
                    BuildOp::Belt => vec![ins[0]],
                };
                resolved[n] = Some((ins, outs));
                progress = true;
            }
            if all_done {
                return Ok(resolved.into_iter().map(|r| r.expect("all done")).collect());
            }
            if !progress {
                let stuck = self
                    .nodes
                    .iter()
                    .zip(&resolved)
                    .find(|(_, r)| r.is_none())
                    .map(|(n, _)| n.label())
                    .unwrap_or("?");
                return Err(format!(
                    "builder '{stuck}' is stuck in a type loop — a structure \
                     cannot be built out of itself; break the cycle"
                ));
            }
        }
    }

    /// Compile: infer builder types, build, intern; returns the net, its
    /// input flows, and every node's concrete (in, out) leg types.
    #[allow(clippy::type_complexity)]
    pub fn build(
        &self,
        lib: &mut Library,
        structs: &mut StructLib,
    ) -> Result<(NetId, Vec<Counting>, Vec<(Vec<ItemType>, Vec<ItemType>)>), String> {
        self.check()?;
        let node_types = self.infer(structs)?;
        // Intern every sealed sub-factory (recursively). Two identical
        // modules come back as the same NetId.
        let mut module_ids: Vec<Option<NetId>> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            module_ids.push(match node {
                DraftNode::Module { label, draft } => Some(
                    draft
                        .build(lib, structs)
                        .map_err(|e| format!("in module '{label}': {e}"))?
                        .0,
                ),
                _ => None,
            });
        }
        let mut b = NetBuilder::new();
        let ins: Vec<_> = self.inputs.iter().map(|i| b.input(i.ty)).collect();
        let mut handles = Vec::with_capacity(self.nodes.len());
        for (n, node) in self.nodes.iter().enumerate() {
            handles.push(match node {
                DraftNode::Recipe { consume, produce, latency, .. } => {
                    let in_tys: Vec<ItemType> = consume.iter().map(|&(t, _)| t).collect();
                    let out_tys: Vec<ItemType> = produce.iter().map(|&(t, _)| t).collect();
                    b.recipe(
                        Recipe::new(
                            consume.iter().map(|&(_, a)| a).collect(),
                            produce.iter().map(|&(_, a)| a).collect(),
                            *latency,
                        ),
                        &in_tys,
                        &out_tys,
                    )
                }
                DraftNode::Priority { item, token, .. } => b.priority(*item, *token),
                DraftNode::Module { .. } => {
                    b.module(lib, module_ids[n].expect("phase one filled this"))
                }
                DraftNode::Builder { op, latency, .. } => {
                    let (in_tys, out_tys) = &node_types[n];
                    let recipe = match op {
                        BuildOp::Weld { .. } => Recipe::new(vec![1, 1], vec![1], *latency),
                        BuildOp::Rot | BuildOp::Belt => {
                            Recipe::new(vec![1], vec![1], *latency)
                        }
                        BuildOp::Split => Recipe::new(vec![2], vec![1, 1], *latency),
                    };
                    b.recipe(recipe, in_tys, out_tys)
                }
            });
        }
        let outs: Vec<_> = self.outputs.iter().map(|o| b.output(o.ty)).collect();

        for (from, to) in &self.wires {
            let src = match *from {
                DraftFrom::Input(i) => ins[i],
                DraftFrom::Node(n, leg) => handles[n].output(leg as u32),
            };
            let sink = match *to {
                DraftTo::Node(n, leg) => handles[n].input(leg as u32),
                DraftTo::Output(o) => outs[o],
            };
            b.connect(src, sink);
        }
        for (to, m) in &self.markings {
            let sink = match *to {
                DraftTo::Node(n, leg) => handles[n].input(leg as u32),
                DraftTo::Output(o) => outs[o],
            };
            b.marking(sink, *m);
        }

        let id = lib.intern(b.build()).map_err(|e| friendly_net_error(&e, self))?;
        let flows = self
            .inputs
            .iter()
            .map(|i| {
                if i.rate.0 == 0 {
                    Counting::zero()
                } else {
                    Counting::unit_rate().scale_floor(i.rate.0, i.rate.1)
                }
            })
            .collect();
        Ok((id, flows, node_types))
    }
}

fn friendly_net_error(e: &NetError, draft: &Draft) -> String {
    match e {
        NetError::SourceUsedTwice(_) => "an output feeds two different wires — items can't \
            be copied; split the flow with a recipe (e.g. 2·x → 1·x + 1·x)"
            .into(),
        NetError::TypeMismatch { wire, .. } => {
            format!("type mismatch on a wire (sink #{wire}) — the item kinds don't agree")
        }
        NetError::PassThroughCycle => {
            "a loop made only of wires — items would circulate with nothing consuming them"
                .into()
        }
        other => {
            let _ = draft;
            format!("invalid blueprint: {other:?}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Evaluator;

    fn iron() -> ItemType {
        ItemType(0)
    }
    fn gear() -> ItemType {
        ItemType(2)
    }

    #[test]
    fn a_minimal_draft_compiles_and_summarizes() {
        let mut d = Draft {
            types: vec![(iron(), "iron".into()), (gear(), "gear".into())],
            ..Default::default()
        };
        d.inputs.push(DraftInput { ty: iron(), label: "mine".into(), rate: (1, 1) });
        d.nodes.push(DraftNode::Recipe {
            label: "gears".into(),
            consume: vec![(iron(), 2)],
            produce: vec![(gear(), 1)],
            latency: 3,
        });
        d.outputs.push(DraftOutput { ty: gear(), label: "out".into() });
        d.wires.push((DraftFrom::Input(0), DraftTo::Node(0, 0)));
        d.wires.push((DraftFrom::Node(0, 0), DraftTo::Output(0)));

        let mut lib = Library::new();
        let mut structs = crate::structure::StructLib::new();
        let (id, flows, _) = d.build(&mut lib, &mut structs).unwrap();
        let mut ev = Evaluator::new(&lib);
        let s = ev.summarize(id, &flows).unwrap();
        assert_eq!(s.outputs[0].rate, (1, 2));
        assert_eq!(s.outputs[0].first, Some(5));
    }

    #[test]
    fn friendly_errors_teach_the_rules() {
        // Copying: one source, two wires.
        let mut d = Draft {
            types: vec![(iron(), "iron".into())],
            ..Default::default()
        };
        d.inputs.push(DraftInput { ty: iron(), label: "mine".into(), rate: (1, 1) });
        d.outputs.push(DraftOutput { ty: iron(), label: "a".into() });
        d.outputs.push(DraftOutput { ty: iron(), label: "b".into() });
        d.wires.push((DraftFrom::Input(0), DraftTo::Output(0)));
        d.wires.push((DraftFrom::Input(0), DraftTo::Output(1)));
        let mut lib = Library::new();
        let err = d.build(&mut lib, &mut crate::structure::StructLib::new()).unwrap_err();
        assert!(err.contains("can't be copied"), "{err}");

        // Dangling wire after a (simulated) deletion.
        let mut d2 = Draft {
            types: vec![(iron(), "iron".into())],
            ..Default::default()
        };
        d2.inputs.push(DraftInput { ty: iron(), label: "mine".into(), rate: (1, 1) });
        d2.wires.push((DraftFrom::Input(0), DraftTo::Node(7, 0)));
        let err = d2
            .build(&mut Library::new(), &mut crate::structure::StructLib::new())
            .unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
    }
}
