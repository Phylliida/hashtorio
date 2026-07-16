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

/// The machine kind, as the chassis registry names it.
pub fn kind_str(node: &DraftNode) -> &'static str {
    match node {
        DraftNode::Recipe { .. } => "recipe",
        DraftNode::Priority { .. } => "priority",
        DraftNode::Module { .. } => "module",
        DraftNode::Builder { op: BuildOp::Weld { .. }, .. } => "weld",
        DraftNode::Builder { op: BuildOp::Rot, .. } => "rot",
        DraftNode::Builder { op: BuildOp::Split, .. } => "split",
        DraftNode::Builder { op: BuildOp::Belt, .. } => "belt",
    }
}

fn footprint_size(structs: &mut StructLib, node: &DraftNode) -> (i32, i32) {
    let ch = crate::structure::chassis(structs, kind_str(node));
    let cells = structs.cells(ch);
    (
        cells.iter().map(|c| c.0).max().unwrap_or(0) + 1,
        cells.iter().map(|c| c.1).max().unwrap_or(0) + 1,
    )
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

/// Items travel `BELT_SPEED` grid cells per tick along wires.
pub const BELT_SPEED: i32 = 2;

#[derive(Debug, Clone, Default)]
pub struct Draft {
    /// The item palette: (id, display name).
    pub types: Vec<(ItemType, String)>,
    pub inputs: Vec<DraftInput>,
    pub outputs: Vec<DraftOutput>,
    pub nodes: Vec<DraftNode>,
    pub wires: Vec<(DraftFrom, DraftTo)>,
    pub markings: Vec<(DraftTo, u64)>,
    /// Factory-space: grid positions, parallel to the lists above. Empty
    /// means "abstract mode" (no geometry: wires instant, no footprints).
    /// When present, geometry is *semantic*: wire latency = distance /
    /// BELT_SPEED, and machine chassis are literal footprints that must
    /// not overlap.
    pub input_pos: Vec<(i32, i32)>,
    pub node_pos: Vec<(i32, i32)>,
    pub output_pos: Vec<(i32, i32)>,
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
        if self.spatial()
            && (self.input_pos.len() != self.inputs.len()
                || self.node_pos.len() != self.nodes.len()
                || self.output_pos.len() != self.outputs.len())
        {
            return Err("positions out of step with parts (internal editor bug)".into());
        }
        Ok(())
    }

    fn spatial(&self) -> bool {
        !(self.input_pos.is_empty() && self.node_pos.is_empty() && self.output_pos.is_empty())
    }

    /// The grid cell where a source endpoint's out-port sits.
    fn out_port_cell(&self, structs: &mut StructLib, from: &DraftFrom) -> (i32, i32) {
        match from {
            DraftFrom::Input(i) => {
                let p = self.input_pos[*i];
                (p.0 + 1, p.1)
            }
            DraftFrom::Node(n, leg) => {
                let p = self.node_pos[*n];
                let (w, _) = footprint_size(structs, &self.nodes[*n]);
                (p.0 + w, p.1 + *leg as i32)
            }
        }
    }

    /// The grid cell where a sink endpoint's in-port sits.
    fn in_port_cell(&self, to: &DraftTo) -> (i32, i32) {
        match to {
            DraftTo::Node(n, leg) => {
                let p = self.node_pos[*n];
                (p.0 - 1, p.1 + *leg as i32)
            }
            DraftTo::Output(o) => {
                let p = self.output_pos[*o];
                (p.0 - 1, p.1)
            }
        }
    }

    /// Wire latency from geometry: Manhattan distance over belt speed.
    fn wire_latency(&self, structs: &mut StructLib, from: &DraftFrom, to: &DraftTo) -> u64 {
        if !self.spatial() {
            return 0;
        }
        let a = self.out_port_cell(structs, from);
        let b = self.in_port_cell(to);
        (((a.0 - b.0).abs() + (a.1 - b.1).abs()) / BELT_SPEED) as u64
    }

    /// Machines are their chassis: footprints must not overlap.
    fn check_footprints(&self, structs: &mut StructLib) -> Result<(), String> {
        if !self.spatial() {
            return Ok(());
        }
        let mut occupied: std::collections::HashMap<(i32, i32), String> =
            std::collections::HashMap::new();
        let mut claim = |cells: Vec<(i32, i32)>, who: &str| -> Result<(), String> {
            for c in cells {
                if let Some(prev) = occupied.insert(c, who.to_string()) {
                    return Err(format!(
                        "'{who}' and '{prev}' overlap at ({}, {}) — machines are their \
                         chassis; give them room",
                        c.0, c.1
                    ));
                }
            }
            Ok(())
        };
        for (i, input) in self.inputs.iter().enumerate() {
            let p = self.input_pos[i];
            claim(vec![p], &input.label)?;
        }
        for (n, node) in self.nodes.iter().enumerate() {
            let p = self.node_pos[n];
            let ch = crate::structure::chassis(structs, kind_str(node));
            let cells = structs
                .cells(ch)
                .iter()
                .map(|&(x, y, _)| (p.0 + x, p.1 + y))
                .collect();
            claim(cells, node.label())?;
        }
        for (o, out) in self.outputs.iter().enumerate() {
            let p = self.output_pos[o];
            claim(vec![p, (p.0, p.1 + 1)], &out.label)?;
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

    /// What this blueprint costs, recursively through module interiors:
    /// machines (one chassis each), belts (one segment per tick of wire
    /// latency — a belt chassis is two cells long, and BELT_SPEED is two
    /// cells per tick), and markings (real items preloaded onto the line).
    /// Machines and belts are capital (need <= own); markings are consumed.
    pub fn cost(&self, structs: &mut StructLib) -> Result<CostReport, String> {
        let mut report = CostReport::default();
        self.cost_into(structs, &mut report)?;
        Ok(report)
    }

    fn cost_into(&self, structs: &mut StructLib, report: &mut CostReport) -> Result<(), String> {
        for node in &self.nodes {
            let ty = crate::structure::chassis(structs, kind_str(node));
            *report.machines.entry(ty).or_insert(0) += 1;
            if let DraftNode::Module { draft, .. } = node {
                draft.cost_into(structs, report)?;
            }
        }
        for (from, to) in &self.wires {
            report.belts += self.wire_latency(structs, from, to);
        }
        for (to, n) in &self.markings {
            let ty = self
                .sink_type(*to)
                .ok_or("a marking sits on a missing port")?;
            if ty == ANY {
                return Err(
                    "preloads on builder ports aren't supported — their item type \
                     depends on wiring; preload a typed port instead"
                        .into(),
                );
            }
            *report.markings.entry(ty).or_insert(0) += n;
        }
        Ok(())
    }

    /// Compile: infer builder types, check footprints, build (inserting a
    /// belt per wire with geometric latency), intern.
    pub fn build(&self, lib: &mut Library, structs: &mut StructLib) -> Result<Built, String> {
        self.check()?;
        self.check_footprints(structs)?;
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
                        .id,
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

        // Distance is time: each wire with geometric latency compiles into
        // an identity belt recipe. Belt nodes are appended after user nodes,
        // so user indices stay 1:1 with the draft.
        let mut wire_lats = Vec::with_capacity(self.wires.len());
        for (from, to) in &self.wires {
            let lat = self.wire_latency(structs, from, to);
            wire_lats.push(lat);
            let src = match *from {
                DraftFrom::Input(i) => ins[i],
                DraftFrom::Node(n, leg) => handles[n].output(leg as u32),
            };
            let sink = match *to {
                DraftTo::Node(n, leg) => handles[n].input(leg as u32),
                DraftTo::Output(o) => outs[o],
            };
            if lat == 0 {
                b.connect(src, sink);
            } else {
                let ty = match *from {
                    DraftFrom::Input(i) => self.inputs[i].ty,
                    DraftFrom::Node(n, leg) => node_types[n].1[leg],
                };
                let belt = b.recipe(Recipe::new(vec![1], vec![1], lat), &[ty], &[ty]);
                b.connect(src, belt.input(0));
                b.connect(belt.output(0), sink);
            }
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
        Ok(Built { id, flows, node_types, wire_lats })
    }
}

/// What a blueprint costs to deploy.
#[derive(Debug, Default)]
pub struct CostReport {
    /// Chassis needed per machine kind (capital: need <= own).
    pub machines: std::collections::HashMap<ItemType, u64>,
    /// Belt segments needed (capital): one per tick of wire latency.
    pub belts: u64,
    /// Items preloaded onto the line (consumed at compile).
    pub markings: std::collections::HashMap<ItemType, u64>,
}

/// The result of compiling a draft.
#[derive(Debug)]
pub struct Built {
    pub id: NetId,
    pub flows: Vec<Counting>,
    /// Concrete (in, out) leg types per draft node (builders resolved).
    #[allow(clippy::type_complexity)]
    pub node_types: Vec<(Vec<ItemType>, Vec<ItemType>)>,
    /// Geometric latency per draft wire (0 = instant/abstract).
    pub wire_lats: Vec<u64>,
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
        let built = d.build(&mut lib, &mut structs).unwrap();
        let (id, flows) = (built.id, built.flows);
        let mut ev = Evaluator::new(&lib);
        let s = ev.summarize(id, &flows).unwrap();
        assert_eq!(s.outputs[0].rate, (1, 2));
        assert_eq!(s.outputs[0].first, Some(5));
    }

    /// Absolute position never enters the compiled net: wire latencies are
    /// Manhattan *differences* and footprint collision is relative, so a
    /// translated factory interns to the same NetId and shares every cache
    /// entry. This is the spatial half of the motion story (DESIGN-motion.md):
    /// summaries are already quotiented by translation, and a uniformly
    /// moving structure is finite modulo (translate ∘ time-shift).
    #[test]
    fn translation_yields_the_same_net_id() {
        let mk = |dx: i32, dy: i32| {
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
            d.markings.push((DraftTo::Node(0, 0), 4));
            d.input_pos = vec![(2 + dx, 3 + dy)];
            d.node_pos = vec![(8 + dx, 3 + dy)];
            d.output_pos = vec![(16 + dx, 5 + dy)];
            d
        };
        let mut lib = Library::new();
        let mut structs = crate::structure::StructLib::new();
        let here = mk(0, 0).build(&mut lib, &mut structs).unwrap();
        let there = mk(7, 13).build(&mut lib, &mut structs).unwrap();
        assert_eq!(here.id, there.id, "translated factory is the same net");
        assert_eq!(here.wire_lats, there.wire_lats, "latencies are relative");
        // And a *non*-translation (stretching a wire) is a different net.
        let mut far = mk(0, 0);
        far.output_pos = vec![(30, 5)];
        let far = far.build(&mut lib, &mut structs).unwrap();
        assert_ne!(here.id, far.id, "stretching a wire changes the net");
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
